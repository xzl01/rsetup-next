const i18n = window.RsetupI18n;
const { t } = i18n;

const routes = [
  { id: "overview", icon: "overview" },
  { id: "system", icon: "system" },
  { id: "network", icon: "network" },
  { id: "hardware", icon: "chip" },
  { id: "workflows", icon: "run" },
  { id: "activity", icon: "pulse" },
];

const workflowGroups = [
  { id: "system", actions: ["system.inspect", "system.update", "power.enable-sleep", "power.disable-sleep", "system.reboot"] },
  { id: "network", actions: ["service.ssh-install", "service.ssh-enable", "service.ssh-disable", "service.ssh-regenerate-host-keys", "service.ssh-remove", "network.restart"] },
  { id: "services", actions: ["service.docker-install", "service.docker-enable", "service.docker-disable", "service.docker-remove"] },
  { id: "storage", actions: ["storage.expand-root"] },
];

const quickActionIds = [
  "system.inspect",
  "system.update",
  "system.change-sources",
  "service.ssh-enable",
  "network.restart",
];

const socVendors = [
  { id: "rockchip", name: "Rockchip", partnerLogo: true, patterns: ["rockchip", "rk35"] },
  { id: "allwinner", name: "Allwinner", partnerLogo: true, patterns: ["allwinner", "sunxi", "sun50"] },
  { id: "cix", name: "CIX", partnerLogo: true, patterns: ["cix", "sky1"] },
  { id: "qualcomm", name: "Qualcomm", partnerLogo: true, patterns: ["qualcomm", "qcom", "snapdragon", "qcs"] },
  { id: "amlogic", name: "Amlogic", partnerLogo: true, patterns: ["amlogic", "a311d"] },
  { id: "broadcom", name: "Broadcom", patterns: ["broadcom", "brcm", "bcm"] },
  { id: "mediatek", name: "MediaTek", partnerLogo: true, patterns: ["mediatek", "mtk"] },
  { id: "nvidia", name: "NVIDIA", patterns: ["nvidia", "tegra"] },
  { id: "nxp", name: "NXP", patterns: ["nxp", "fsl,", "imx"] },
  { id: "starfive", name: "StarFive", partnerLogo: true, patterns: ["starfive", "jh71"] },
  { id: "sophgo", name: "Sophgo", patterns: ["sophgo", "cv18"] },
  { id: "intel", name: "Intel", partnerLogo: true, patterns: ["intel"] },
];

const capabilityVisuals = {
  "device-tree": { icon: "device-tree", tone: "signal" },
  gpio: { icon: "gpio", tone: "emerald" },
  video: { icon: "video", tone: "cyan" },
  thermal: { icon: "thermal", tone: "amber" },
  "spi-flash": { icon: "spi-flash", tone: "signal" },
};

const debugDeviceProfiles = [
  { id: "rockchip-rk3588", label: "Rockchip · RK3588", product: "Rockchip RK3588 Demo", hostname: "debug-rk3588", socVendor: "Rockchip", soc: "RK3588", architecture: "aarch64" },
  { id: "allwinner-h618", label: "Allwinner · H618", product: "Allwinner H618 Demo", hostname: "debug-h618", socVendor: "Allwinner", soc: "H618", architecture: "aarch64" },
  { id: "cix-p1", label: "CIX · P1", product: "CIX P1 Demo", hostname: "debug-cix-p1", socVendor: "CIX", soc: "P1", architecture: "aarch64" },
  { id: "qualcomm-qcs6490", label: "Qualcomm · QCS6490", product: "Qualcomm QCS6490 Demo", hostname: "debug-qcs6490", socVendor: "Qualcomm", soc: "QCS6490", architecture: "aarch64" },
  { id: "amlogic-a311d", label: "Amlogic · A311D", product: "Amlogic A311D Demo", hostname: "debug-a311d", socVendor: "Amlogic", soc: "A311D", architecture: "aarch64" },
  { id: "mediatek-genio700", label: "MediaTek · Genio 700", product: "MediaTek Genio 700 Demo", hostname: "debug-genio700", socVendor: "MediaTek", soc: "Genio 700", architecture: "aarch64" },
  { id: "starfive-jh7110", label: "StarFive · JH7110", product: "StarFive JH7110 Demo", hostname: "debug-jh7110", socVendor: "StarFive", soc: "JH7110", architecture: "riscv64" },
];

const defaultDebugCustom = {
  product: "Custom SBC Demo",
  hostname: "debug-sbc",
  socVendor: "Rockchip",
  soc: "RK3588",
  architecture: "aarch64",
};

const state = {
  providerSnapshot: null,
  snapshot: null,
  actions: [],
  activity: [],
  sources: null,
  sourcePlan: null,
  selectedAction: null,
  lastInvoker: null,
  route: "overview",
  refreshing: false,
  debugProfile: "provider",
  debugCustom: { ...defaultDebugCustom },
};

const $ = (selector, scope = document) => scope.querySelector(selector);
const $$ = (selector, scope = document) => Array.from(scope.querySelectorAll(selector));
const tauriInvoke = window.__TAURI__?.core?.invoke;

const transport = {
  async snapshot() {
    if (tauriInvoke) return tauriInvoke("system_snapshot");
    return request("/api/v1/snapshot");
  },
  async actions() {
    if (tauriInvoke) return tauriInvoke("list_actions");
    return request("/api/v1/actions");
  },
  async activity() {
    if (tauriInvoke) return tauriInvoke("list_activity");
    return request("/api/v1/activity");
  },
  async runAction(actionId, confirm) {
    if (tauriInvoke) return tauriInvoke("run_action", { actionId, confirm });
    return request(`/api/v1/actions/${encodeURIComponent(actionId)}/run`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ confirm }),
    });
  },
  async sourceStatus() {
    if (tauriInvoke) return tauriInvoke("source_status");
    return request("/api/v1/sources");
  },
  async planSources(providerId) {
    if (tauriInvoke) return tauriInvoke("plan_sources", { providerId });
    return request("/api/v1/sources/plan", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ providerId }),
    });
  },
  async applySources(providerId, planToken, confirm) {
    if (tauriInvoke) return tauriInvoke("apply_sources", { providerId, planToken, confirm });
    return request("/api/v1/sources/apply", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ providerId, planToken, confirm }),
    });
  },
};

async function request(path, options) {
  let response;
  try {
    response = await fetch(path, options);
  } catch {
    const error = new Error(t("api.transport_failure"));
    error.localized = true;
    throw error;
  }
  const body = await response.json().catch(() => ({}));
  if (!response.ok) {
    const message = body.error?.code
      ? i18n.apiError(body.error.code, body.error.message)
      : t("api.http_failure", { status: response.status });
    const error = new Error(message);
    error.code = body.error?.code;
    error.localized = true;
    throw error;
  }
  return body;
}

function displayError(error) {
  if (error?.localized) return error.message;
  if (error?.code) return i18n.apiError(error.code, error.message);
  if (i18n.getLocale() === "zh-CN") return t("api.internal_error");
  return error?.message || String(error);
}

function setText(selector, value, scope = document) {
  const element = $(selector, scope);
  if (element) element.textContent = value;
}

function icon(name) {
  return `<svg aria-hidden="true"><use href="#icon-${name}"></use></svg>`;
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function formatNumber(value, digits = 1) {
  return new Intl.NumberFormat(i18n.getLocale(), {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  }).format(Number(value || 0));
}

function formatPercent(value) {
  return `${formatNumber(value, 1)}%`;
}

function byteUnit(value) {
  const bytes = Number(value || 0);
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let number = bytes / 1024;
  let index = 0;
  while (number >= 1024 && index < units.length - 1) {
    number /= 1024;
    index += 1;
  }
  return `${formatNumber(number, number >= 10 ? 1 : 2)} ${units[index]}`;
}

function duration(seconds) {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days) return t("duration.days", { days, hours });
  if (hours) return t("duration.hours", { hours, minutes });
  return t("duration.minutes", { minutes });
}

function relativeTime(date) {
  const delta = Math.max(0, Date.now() - new Date(date).getTime());
  if (delta < 60000) return t("relative.now");
  if (delta < 3600000) return t("relative.minutes", { count: Math.floor(delta / 60000) });
  if (delta < 86400000) return t("relative.hours", { count: Math.floor(delta / 3600000) });
  return t("relative.days", { count: Math.floor(delta / 86400000) });
}

function riskLabel(risk) {
  return t(`risk.${risk}`);
}

function socVendor(identity) {
  const explicit = identity.socVendor?.trim() || "";
  const haystack = `${explicit} ${identity.soc || ""}`.toLocaleLowerCase("en");
  const known = socVendors.find((vendor) => vendor.patterns.some((pattern) => haystack.includes(pattern)));
  if (known) return known;
  const name = explicit || "SoC";
  return { id: "generic", name };
}

function socDisplayName(identity, vendor) {
  const soc = identity.soc || t("provider.unknown");
  return vendor.id === "generic" || soc.toLocaleLowerCase("en").includes(vendor.name.toLocaleLowerCase("en"))
    ? soc
    : `${vendor.name} ${soc}`;
}

function capabilityVisual(id) {
  return capabilityVisuals[id] || { icon: "chip", tone: "signal" };
}

function debugProfileById(id) {
  return debugDeviceProfiles.find((profile) => profile.id === id);
}

function loadDebugState() {
  try {
    const storedProfile = localStorage.getItem("rsetup-debug-profile-v1");
    if (storedProfile === "provider" || storedProfile === "custom" || debugProfileById(storedProfile)) state.debugProfile = storedProfile;
    const storedCustom = JSON.parse(localStorage.getItem("rsetup-debug-custom-v1") || "null");
    if (storedCustom && typeof storedCustom === "object") {
      state.debugCustom = Object.fromEntries(
        Object.entries(defaultDebugCustom).map(([key, fallback]) => [
          key,
          typeof storedCustom[key] === "string" ? storedCustom[key] : fallback,
        ]),
      );
    }
  } catch { /* debug persistence is optional */ }
}

function saveDebugState() {
  try {
    localStorage.setItem("rsetup-debug-profile-v1", state.debugProfile);
    localStorage.setItem("rsetup-debug-custom-v1", JSON.stringify(state.debugCustom));
  } catch { /* debug persistence is optional */ }
}

function applyDebugDevice(snapshot) {
  if (!snapshot?.synthetic || state.debugProfile === "provider") return snapshot;
  const profile = state.debugProfile === "custom" ? state.debugCustom : debugProfileById(state.debugProfile);
  if (!profile) return snapshot;
  return {
    ...snapshot,
    identity: {
      ...snapshot.identity,
      id: `debug-${state.debugProfile}`,
      product: profile.product,
      hostname: profile.hostname,
      socVendor: profile.socVendor,
      soc: profile.soc,
      architecture: profile.architecture,
    },
  };
}

function renderDebugControls() {
  const menu = $("[data-debug-menu]");
  const synthetic = Boolean(state.providerSnapshot?.synthetic);
  menu.hidden = !synthetic;
  if (!synthetic) {
    menu.open = false;
    return;
  }
  menu.dataset.active = String(state.debugProfile !== "provider");
  const select = $("[data-debug-profile]");
  select.innerHTML = [
    `<option value="provider">${escapeHtml(t("debug.provider"))}</option>`,
    ...debugDeviceProfiles.map((profile) => `<option value="${escapeHtml(profile.id)}">${escapeHtml(profile.label)}</option>`),
    `<option value="custom">${escapeHtml(t("debug.custom"))}</option>`,
  ].join("");
  select.value = state.debugProfile;
  const custom = $("[data-debug-custom]");
  custom.hidden = state.debugProfile !== "custom";
  if (!custom.contains(document.activeElement)) {
    $("[data-debug-product]").value = state.debugCustom.product;
    $("[data-debug-vendor]").value = state.debugCustom.socVendor;
    $("[data-debug-soc]").value = state.debugCustom.soc;
    $("[data-debug-architecture]").value = state.debugCustom.architecture;
    $("[data-debug-hostname]").value = state.debugCustom.hostname;
  }
}

function activateDebugProfile(profileId) {
  state.debugProfile = profileId === "custom" || profileId === "provider" || debugProfileById(profileId) ? profileId : "provider";
  saveDebugState();
  state.snapshot = applyDebugDevice(state.providerSnapshot);
  renderSnapshot();
  renderDebugControls();
  resolveSignals();
  if (state.debugProfile !== "custom") {
    toast(t("debug.title"), t("debug.changed", { device: state.snapshot.identity.product }));
  }
}

function applyCustomDebugDevice() {
  state.debugCustom = {
    product: $("[data-debug-product]").value.trim() || defaultDebugCustom.product,
    socVendor: $("[data-debug-vendor]").value.trim() || defaultDebugCustom.socVendor,
    soc: $("[data-debug-soc]").value.trim() || defaultDebugCustom.soc,
    architecture: $("[data-debug-architecture]").value.trim() || defaultDebugCustom.architecture,
    hostname: $("[data-debug-hostname]").value.trim() || defaultDebugCustom.hostname,
  };
  state.debugProfile = "custom";
  saveDebugState();
  state.snapshot = applyDebugDevice(state.providerSnapshot);
  renderSnapshot();
  renderDebugControls();
  resolveSignals();
  toast(t("debug.title"), t("debug.changed", { device: state.debugCustom.product }));
}

function applyStaticTranslations() {
  document.title = t("document.title");
  $("meta[name='description']")?.setAttribute("content", t("document.description"));
  $$('[data-i18n]').forEach((element) => { element.textContent = t(element.dataset.i18n); });
  $$('[data-i18n-aria]').forEach((element) => { element.setAttribute("aria-label", t(element.dataset.i18nAria)); });
  $$('[data-i18n-placeholder]').forEach((element) => { element.setAttribute("placeholder", t(element.dataset.i18nPlaceholder)); });
  setText("[data-language-label]", t("language.short"));
}

function openDialog(dialog) {
  if (typeof dialog.showModal === "function") dialog.showModal();
  else dialog.setAttribute("open", "");
}

function dismissDialog(dialog) {
  if (typeof dialog.close === "function") dialog.close();
  else dialog.removeAttribute("open");
}

async function refreshAll({ quiet = false } = {}) {
  if (state.refreshing) return;
  const shouldResolve = !state.snapshot || !quiet;
  state.refreshing = true;
  document.body.dataset.state = "loading";
  setText("[data-footer-status]", t("status.syncing"));
  setText("[data-footer-detail]", t("status.reading"));
  try {
    const previousSourceRevision = state.sources?.sourceRevision;
    const [snapshot, actions, activity, sources] = await Promise.all([
      transport.snapshot(), transport.actions(), transport.activity(), transport.sourceStatus(),
    ]);
    state.providerSnapshot = snapshot;
    state.snapshot = applyDebugDevice(snapshot);
    state.actions = actions;
    state.activity = activity;
    const sourceChanged = previousSourceRevision
      && previousSourceRevision !== sources.sourceRevision;
    state.sources = sources;
    if (sourceChanged && state.sourcePlan) clearSourcePlan();
    renderAll();
    document.body.dataset.state = snapshot.synthetic ? "demo" : "live";
    if (shouldResolve) resolveSignals();
    if (!quiet) toast(t("toast.probeComplete"), snapshot.synthetic ? t("toast.demoLoaded") : t("toast.localCurrent"));
  } catch (error) {
    const detail = displayError(error);
    document.body.dataset.state = "error";
    setText("[data-footer-status]", t("status.unavailable"));
    setText("[data-footer-detail]", detail);
    toast(t("toast.refreshFailed"), detail, true);
  } finally {
    state.refreshing = false;
  }
}

function resolveSignals() {
  document.body.classList.remove("is-resolving");
  void document.body.offsetWidth;
  document.body.classList.add("is-resolving");
  window.setTimeout(() => document.body.classList.remove("is-resolving"), 950);
}

function renderAll() {
  renderSnapshot();
  renderActions();
  renderActivity();
  renderSources();
  renderCommandResults();
  renderDebugControls();
}

function providerName(providerId) {
  if (!providerId) return t("provider.unknown");
  if (providerId === "official") return t("provider.official");
  if (providerId === "mixed") return t("provider.mixed");
  return state.sources?.providers.find((provider) => provider.id === providerId)?.name || providerId;
}

function providerLocation(location) {
  const key = {
    "Global": "global",
    "China": "china",
    "Hefei, CN": "hefei",
    "Beijing, CN": "beijing",
    "Chongqing, CN": "chongqing",
    "Lanzhou, CN": "lanzhou",
    "Wuhan, CN": "wuhan",
    "Jinan, CN": "jinan",
    "Nanjing, CN": "nanjing",
    "Nanyang, CN": "nanyang",
  }[location];
  return key ? t(`location.${key}`) : location;
}

function renderSources() {
  const source = state.sources;
  const status = $$('[data-source-status] dd');
  const select = $("[data-source-provider]");
  const preview = $("[data-source-preview]");
  if (!source) {
    status.forEach((element) => { element.textContent = t("sources.unavailable"); });
    select.disabled = true;
    preview.disabled = true;
    return;
  }
  const values = [
    `${source.distributionName} · ${source.architecture}`,
    providerName(source.currentSystemProvider),
    providerName(source.currentRadxaProvider),
    String(source.files.length),
  ];
  status.forEach((element, index) => { element.textContent = values[index] || "—"; });

  const previous = select.value;
  select.innerHTML = source.providers.map((provider) => `<option value="${escapeHtml(provider.id)}">${escapeHtml(providerName(provider.id))} · ${escapeHtml(providerLocation(provider.location))}</option>`).join("");
  const preferred = source.providers.some((provider) => provider.id === previous)
    ? previous
    : source.currentSystemProvider && source.currentSystemProvider !== "mixed"
      ? source.currentSystemProvider
      : "official";
  select.value = preferred;
  select.disabled = !source.supported;
  preview.disabled = !source.supported;
  renderProviderDetail();
  if (state.sourcePlan) renderSourcePlan(state.sourcePlan, { preserveConfirmation: true });
}

function renderProviderDetail() {
  const providerId = $("[data-source-provider]")?.value;
  const provider = state.sources?.providers.find((item) => item.id === providerId);
  if (!provider) return;
  setText("[data-source-provider-detail]", t("sources.providerScope", {
    location: providerLocation(provider.location),
    system: provider.systemEndpoint ? t("sources.supported") : t("sources.unchanged"),
    radxa: provider.radxaEndpoint ? t("sources.supported") : t("sources.unchanged"),
  }));
}

function clearSourcePlan() {
  state.sourcePlan = null;
  const host = $("[data-source-plan]");
  const confirm = $("[data-source-confirm]");
  const apply = $("[data-source-apply]");
  if (host) host.hidden = true;
  if (confirm) confirm.checked = false;
  if (apply) apply.disabled = true;
  const result = $("[data-source-result]");
  if (result) {
    result.hidden = true;
    result.classList.remove("is-error");
  }
}

function renderSourcePlan(plan, { preserveConfirmation = false } = {}) {
  const host = $("[data-source-plan]");
  const confirm = $("[data-source-confirm]");
  const confirmed = preserveConfirmation && confirm.checked;
  host.hidden = false;
  setText("[data-source-plan-title]", providerName(plan.provider.id));
  const entries = plan.changes.reduce((total, change) => total + change.replacements, 0);
  setText("[data-source-plan-count]", t("sources.changeCount", { entries, files: plan.changes.length }));
  $("[data-source-diff-list]").innerHTML = plan.changes.length ? plan.changes.map((change) => {
    const lines = change.before.flatMap((before, index) => [
      `<span class="diff-remove">- ${escapeHtml(before)}</span>`,
      `<span class="diff-add">+ ${escapeHtml(change.after[index] || "")}</span>`,
    ]).join("\n");
    return `<article class="source-diff"><header><strong>${escapeHtml(change.path)}</strong><span>${escapeHtml(t("sources.replacements", { count: change.replacements }))}</span></header><pre>${lines}</pre></article>`;
  }).join("") : `<div class="loading-row"><span></span>${escapeHtml(t("sources.noChanges"))}</div>`;
  $("[data-source-warnings]").innerHTML = plan.warnings.map((warning) => `<div class="source-warning">${escapeHtml(t(`sources.warning.${warning}`))}</div>`).join("");
  confirm.checked = confirmed;
  $("[data-source-apply]").disabled = !confirmed || !plan.changes.length;
}

async function previewSources() {
  const providerId = $("[data-source-provider]").value;
  const button = $("[data-source-preview]");
  button.disabled = true;
  $("span", button).textContent = t("sources.previewing");
  try {
    const plan = await transport.planSources(providerId);
    state.sourcePlan = plan;
    renderSourcePlan(plan);
    $("[data-source-plan]").scrollIntoView({ behavior: "smooth", block: "nearest" });
  } catch (error) {
    const detail = displayError(error);
    toast(t("toast.failed"), detail, true);
  } finally {
    $("span", button).textContent = t("sources.preview");
    button.disabled = !state.sources?.supported;
  }
}

async function applySourcePlan() {
  const plan = state.sourcePlan;
  if (!plan || !$("[data-source-confirm]").checked) return;
  const button = $("[data-source-apply]");
  const result = $("[data-source-result]");
  button.disabled = true;
  $("span", button).textContent = t("sources.applying");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("sources.runState");
  try {
    const applied = await transport.applySources(plan.provider.id, plan.planToken, true);
    const heading = applied.run.synthetic
      ? t("sources.planned")
      : applied.rolledBack
        ? t("sources.rolledBack")
        : t("sources.applied");
    result.classList.toggle("is-error", applied.run.status === "failed");
    const rawOutput = applied.run.output && !applied.run.synthetic ? `\n\n${applied.run.output}` : "";
    result.textContent = `${heading}\n${i18n.runSummary(applied.run)}${applied.backups.length ? `\n${t("sources.backups", { count: applied.backups.length })}` : ""}${rawOutput}`;
    toast(heading, i18n.runSummary(applied.run), applied.run.status === "failed");
    await refreshAll({ quiet: true });
  } catch (error) {
    const detail = displayError(error);
    if (error?.code === "stale_plan" || error?.code === "plan_required") {
      state.sourcePlan = null;
      $("[data-source-confirm]").checked = false;
    }
    result.classList.add("is-error");
    result.textContent = `${t("drawer.failed")}\n${detail}`;
    toast(t("toast.failed"), detail, true);
  } finally {
    $("span", button).textContent = t("sources.apply");
    button.disabled = !$("[data-source-confirm]").checked || !state.sourcePlan?.changes.length;
  }
}

function renderSnapshot() {
  const snapshot = state.snapshot;
  if (!snapshot) return;
  const { identity, metrics, storage, interfaces, services, capabilities } = snapshot;
  const memoryPercent = metrics.memoryTotalBytes ? metrics.memoryUsedBytes / metrics.memoryTotalBytes * 100 : 0;
  const root = storage.find((item) => item.mountPoint === "/") || storage[0];
  const rootPercent = root?.totalBytes ? root.usedBytes / root.totalBytes * 100 : 0;
  const vendor = socVendor(identity);
  const socName = socDisplayName(identity, vendor);

  setText("[data-product]", identity.product);
  setText("[data-hostname]", identity.hostname);
  const emblem = $("[data-soc-emblem]");
  emblem.dataset.vendor = vendor.id;
  emblem.dataset.logo = vendor.partnerLogo ? "partner" : "generic";
  emblem.setAttribute("aria-label", t("soc.vendorLabel", { vendor: vendor.name }));
  setText("[data-soc-vendor]", vendor.name);
  setText("[data-core-state]", snapshot.synthetic ? t("core.demo") : t("core.online"));
  setText("[data-core-detail]", `${socName} · ${identity.architecture}`);
  setText("[data-cpu]", formatPercent(metrics.cpuPercent));
  setText("[data-memory]", formatPercent(memoryPercent));
  setText("[data-temperature]", metrics.temperatureC == null ? "N/A" : `${formatNumber(metrics.temperatureC, 1)} °C`);
  setText("[data-thermal-detail]", metrics.temperatureC == null ? t("temperature.none") : metrics.temperatureC < 70 ? t("temperature.normal") : t("temperature.hot"));
  setText("[data-storage]", root ? formatPercent(rootPercent) : "N/A");
  setText("[data-storage-detail]", root ? `${byteUnit(root.usedBytes)} / ${byteUnit(root.totalBytes)}` : t("storage.unavailable"));
  setText("[data-uptime]", duration(metrics.uptimeSeconds));
  setText("[data-kernel]", identity.kernel);
  setText("[data-arch]", identity.architecture);
  setText("[data-collected]", t("updated", { time: relativeTime(snapshot.collectedAt) }));
  $("[data-cpu-meter]").style.transform = `scaleX(${Math.min(100, metrics.cpuPercent) / 100})`;
  $("[data-memory-meter]").style.transform = `scaleX(${Math.min(100, memoryPercent) / 100})`;

  const stamp = $("[data-mode-stamp]");
  $("strong", stamp).textContent = snapshot.synthetic ? t("status.demoOnline") : t("local.device");
  $("strong", stamp).style.color = snapshot.synthetic ? "var(--amber)" : "var(--signal)";
  setText("[data-footer-status]", snapshot.synthetic ? t("status.demoOnline") : t("status.localStable"));
  setText("[data-footer-detail]", snapshot.synthetic ? t("status.demoDetail") : t("status.localDetail", {
    product: identity.product,
    networks: interfaces.length,
    capabilities: capabilities.filter((item) => item.available).length,
  }));

  setText("[data-capability-count]", t("capabilities.online", {
    available: capabilities.filter((item) => item.available).length,
    total: capabilities.length,
  }));
  $("[data-capabilities]").innerHTML = capabilities.map((raw) => {
    const capability = i18n.capability(raw);
    const visual = capabilityVisual(raw.id);
    return `<div class="capability-signal ${capability.available ? "" : "is-offline"}">
      <span class="capability-mark" data-tone="${visual.tone}" aria-hidden="true">${icon(visual.icon)}</span>
      <strong>${escapeHtml(capability.label)}</strong><span class="capability-detail">${escapeHtml(capability.detail)}</span>
    </div>`;
  }).join("");

  $("[data-services]").innerHTML = services.length ? services.map((raw) => {
    const service = i18n.service(raw);
    return `<div class="data-row"><b>${escapeHtml(service.label)}</b><span class="state-text" data-state="${escapeHtml(raw.state)}">${escapeHtml(i18n.enumLabel("state", raw.state))}</span><span>${escapeHtml(service.detail)}</span></div>`;
  }).join("") : emptyRow(t("empty.services"));

  $("[data-storage-list]").innerHTML = storage.length ? storage.map((disk) => {
    const used = disk.totalBytes ? disk.usedBytes / disk.totalBytes * 100 : 0;
    return `<div class="data-row"><b>${escapeHtml(disk.mountPoint)}</b><span>${escapeHtml(t("storage.used", { percent: formatPercent(used) }))}</span><span>${escapeHtml(t("storage.of", { used: byteUnit(disk.usedBytes), total: byteUnit(disk.totalBytes), name: disk.name }))}</span></div>`;
  }).join("") : emptyRow(t("empty.storage"));

  $("[data-identity]").innerHTML = [
    [t("identity.product"), identity.product], [t("identity.hostname"), identity.hostname],
    [t("identity.soc"), socName], [t("identity.system"), identity.operatingSystem],
    [t("identity.kernel"), identity.kernel], [t("identity.nodeId"), identity.id],
  ].map(([label, value]) => `<dt>${escapeHtml(label)}</dt><dd>${escapeHtml(value)}</dd>`).join("");

  $("[data-network-map]").innerHTML = interfaces.length ? interfaces.map((iface) => `
    <article class="interface-lane">
      <span class="risk-plate" data-risk="${iface.state === "online" || iface.state === "up" ? "safe" : "guarded"}">${escapeHtml(i18n.enumLabel("network", iface.state))}</span>
      <h2>${escapeHtml(iface.name)}</h2><div class="interface-path" aria-hidden="true"></div>
      <div class="interface-facts">
        <span>${t("network.address")}<b>${escapeHtml(iface.address || t("network.notAssigned"))}</b></span>
        <span>${t("network.type")}<b>${escapeHtml(i18n.enumLabel("kind", iface.kind))}</b></span>
        <span>${t("network.received")}<b>${byteUnit(iface.receivedBytes)}</b></span>
        <span>${t("network.transmitted")}<b>${byteUnit(iface.transmittedBytes)}</b></span>
      </div>
    </article>`).join("") : emptyRow(t("empty.network"));

  $("[data-hardware-matrix]").innerHTML = capabilities.map((raw) => {
    const capability = i18n.capability(raw);
    const visual = capabilityVisual(raw.id);
    return `<article class="hardware-cell ${capability.available ? "" : "is-offline"}" data-tone="${visual.tone}">${icon(visual.icon)}<h2>${escapeHtml(capability.label)}</h2><p>${escapeHtml(capability.detail)}</p></article>`;
  }).join("");
}

function renderActions() {
  const actions = state.actions;
  const quick = quickActionIds
    .map((actionId) => actions.find((action) => action.id === actionId))
    .filter(Boolean);
  $("[data-operation-list]").innerHTML = quick.length ? quick.map(operationButton).join("") : emptyRow(t("operations.empty"));
  const detailed = actions.filter((action) => action.id !== "system.change-sources");
  const knownActionIds = new Set(workflowGroups.flatMap((group) => group.actions));
  const groups = workflowGroups.map((group) => ({
    id: group.id,
    actions: detailed.filter((action) => group.actions.includes(action.id)),
  }));
  const other = detailed.filter((action) => !knownActionIds.has(action.id));
  if (other.length) groups.push({ id: "other", actions: other });
  const visibleGroups = groups.filter((group) => group.actions.length);
  $("[data-workflow-table]").innerHTML = visibleGroups.length ? visibleGroups.map((group) => `
    <section class="workflow-group" data-group="${escapeHtml(group.id)}" aria-labelledby="workflow-group-${escapeHtml(group.id)}">
      <div class="workflow-group-head">
        <h2 id="workflow-group-${escapeHtml(group.id)}">${escapeHtml(t(`workflows.group.${group.id}`))}</h2>
        <span>${escapeHtml(t("workflows.group.count", { count: group.actions.length }))}</span>
      </div>
      <div class="workflow-group-list">${group.actions.map(workflowButton).join("")}</div>
    </section>`).join("") : `<div class="workflow-empty">${emptyRow(t("operations.empty"))}</div>`;
  bindActionButtons();
}

function workflowButton(raw) {
  const action = i18n.action(raw);
  const unavailable = !action.available;
  const meta = unavailable ? t("operations.unavailable") : `~${action.estimatedSeconds}s`;
  return `<button class="workflow-row${unavailable ? " is-unavailable" : ""}" type="button" data-action-id="${escapeHtml(action.id)}"${unavailable ? " disabled" : ""}${action.unavailableReason ? ` title="${escapeHtml(action.unavailableReason)}"` : ""}>
    <span class="risk-plate" data-risk="${escapeHtml(action.risk)}">${escapeHtml(riskLabel(action.risk))}</span>
    <span class="workflow-copy"><strong>${escapeHtml(action.title)}</strong>${unavailable ? `<span>${escapeHtml(action.unavailableReason || t("operations.unavailable"))}</span>` : ""}</span>
    <span class="workflow-meta">${escapeHtml(meta)}</span>${icon("run")}
  </button>`;
}

function operationButton(raw) {
  const action = i18n.action(raw);
  const unavailable = !action.available;
  const meta = unavailable ? action.unavailableReason || t("operations.unavailable") : `${action.category} · ${t("steps", { count: action.steps.length })} · ~${action.estimatedSeconds}s`;
  return `<button class="operation-row${unavailable ? " is-unavailable" : ""}" type="button" data-action-id="${escapeHtml(action.id)}"${unavailable ? " disabled" : ""}${action.unavailableReason ? ` title="${escapeHtml(action.unavailableReason)}"` : ""}>
    <span class="risk-plate" data-risk="${escapeHtml(action.risk)}">${escapeHtml(riskLabel(action.risk))}</span>
    <span class="operation-copy"><strong>${escapeHtml(action.title)}</strong><span>${escapeHtml(meta)}</span></span>${icon("run")}
  </button>`;
}

function renderActivity() {
  const activity = state.activity;
  const compact = activity.slice(0, 3);
  $("[data-event-ledger]").innerHTML = compact.length ? compact.map(eventRow).join("") : emptyListItem(t("activity.empty"));
  $("[data-activity-timeline]").innerHTML = activity.length ? activity.map(eventRow).join("") : emptyListItem(t("activity.empty"));
}

function eventRow(raw) {
  const event = i18n.activity(raw);
  return `<li class="event-row"><time datetime="${escapeHtml(event.at)}">${relativeTime(event.at)}</time><strong>${escapeHtml(event.title)}</strong><span>${escapeHtml(event.detail)}${event.synthetic ? ` · ${t("activity.simulated")}` : ""}</span></li>`;
}

function emptyRow(message) {
  return `<div class="loading-row"><span></span>${escapeHtml(message)}</div>`;
}

function emptyListItem(message) {
  return `<li class="loading-row"><span></span>${escapeHtml(message)}</li>`;
}

function navigate(route) {
  if (!routes.some((candidate) => candidate.id === route)) return;
  state.route = route;
  $$('[data-route]').forEach((button) => {
    const active = button.dataset.route === route;
    button.classList.toggle("is-active", active);
    if (button.classList.contains("route-button")) active ? button.setAttribute("aria-current", "page") : button.removeAttribute("aria-current");
  });
  $$('[data-view]').forEach((view) => view.classList.toggle("is-active", view.dataset.view === route));
  $(".workspace").scrollTo({ top: 0, behavior: "instant" });
  history.replaceState(null, "", `#${route}`);
  const dialog = $("[data-command-dialog]");
  if (dialog.open) dismissDialog(dialog);
}

function renderActionDetails({ reset = false } = {}) {
  const raw = state.selectedAction;
  if (!raw) return;
  const action = i18n.action(raw);
  setText("[data-task-title]", action.title);
  setText("[data-task-description]", action.available
    ? action.description
    : `${action.description} ${t("operations.unavailableReason", { reason: action.unavailableReason || t("operations.unavailable") })}`);
  setText("[data-task-time]", `${action.estimatedSeconds}s`);
  setText("[data-task-root]", action.requiresRoot ? t("drawer.root") : t("drawer.user"));
  const risk = $("[data-task-risk]");
  risk.dataset.risk = action.risk;
  risk.textContent = riskLabel(action.risk);
  $("[data-task-steps]").innerHTML = action.steps.map((step) => `<li>${escapeHtml(step)}</li>`).join("");
  const guarded = action.risk !== "safe";
  $("[data-confirm-line]").hidden = !guarded || !action.available;
  if (reset) {
    $("[data-task-confirm]").checked = false;
    $("[data-task-execute]").disabled = !action.available || guarded;
    const result = $("[data-task-result]");
    result.hidden = true;
    result.classList.remove("is-error");
  }
}

function openAction(actionId) {
  const action = state.actions.find((candidate) => candidate.id === actionId);
  if (!action) return;
  if (!action.available) {
    toast(i18n.action(action).title, i18n.action(action).unavailableReason || t("operations.unavailable"), true);
    return;
  }
  if (action.id === "system.change-sources") {
    navigate("workflows");
    const manager = $("#source-manager");
    manager.scrollIntoView({ behavior: "smooth", block: "start" });
    window.setTimeout(() => manager.focus({ preventScroll: true }), 250);
    toast(t("sources.title"), t("sources.focused"));
    return;
  }
  state.selectedAction = action;
  state.lastInvoker = document.activeElement;
  renderActionDetails({ reset: true });
  const drawer = $("[data-task-drawer]");
  if (!drawer.open) openDialog(drawer);
  requestAnimationFrame(() => { drawer.classList.add("is-open"); $("[data-task-close]").focus(); });
}

function closeAction() {
  const drawer = $("[data-task-drawer]");
  drawer.classList.remove("is-open");
  window.setTimeout(() => {
    if (drawer.open) dismissDialog(drawer);
    state.lastInvoker?.focus?.();
    state.lastInvoker = null;
  }, 320);
  state.selectedAction = null;
}

async function executeSelectedAction() {
  const action = state.selectedAction;
  if (!action || !action.available) return;
  const button = $("[data-task-execute]");
  const result = $("[data-task-result]");
  const confirm = action.risk === "safe" || $("[data-task-confirm]").checked;
  button.disabled = true;
  $("span", button).textContent = t("drawer.running");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("drawer.runState");
  try {
    const run = await transport.runAction(action.id, confirm);
    const translatedAction = i18n.action(action);
    const output = run.synthetic && i18n.getLocale() === "zh-CN"
      ? `${t("run.plannedSteps")}:\n- ${translatedAction.steps.join("\n- ")}`
      : run.output;
    result.textContent = `${run.synthetic ? t("drawer.dryRun") : t("drawer.result")} / ${t(`run.status.${run.status}`)}\n${i18n.runSummary(run)}${output ? `\n\n${output}` : ""}`;
    toast(run.synthetic ? t("toast.dryRun") : t("toast.complete"), i18n.runSummary(run));
    await refreshAll({ quiet: true });
  } catch (error) {
    const detail = displayError(error);
    result.classList.add("is-error");
    result.textContent = `${t("drawer.failed")}\n${detail}`;
    toast(t("toast.failed"), detail, true);
  } finally {
    $("span", button).textContent = t("drawer.run");
    button.disabled = !action.available || (action.risk !== "safe" && !$("[data-task-confirm]").checked);
  }
}

function bindActionButtons() {
  $$('[data-action-id]').forEach((button) => button.addEventListener("click", () => openAction(button.dataset.actionId)));
}

function openCommands() {
  const dialog = $("[data-command-dialog]");
  openDialog(dialog);
  const input = $("[data-command-input]");
  input.value = "";
  renderCommandResults();
  requestAnimationFrame(() => input.focus());
}

function renderCommandResults() {
  const host = $("[data-command-results]");
  if (!host) return;
  const query = $("[data-command-input]")?.value.trim().toLocaleLowerCase(i18n.getLocale()) || "";
  const routeItems = routes.map((route) => ({ ...route, label: t(`route.${route.id}`), detail: t(`route.${route.id}.detail`) }))
    .filter((route) => `${route.label} ${route.detail}`.toLocaleLowerCase(i18n.getLocale()).includes(query))
    .map((route) => `<button class="command-result" type="button" data-command-route="${route.id}">${icon(route.icon)}<span><strong>${escapeHtml(route.label)}</strong><span>${escapeHtml(route.detail)}</span></span><em>${t("command.section")}</em></button>`);
  const actionItems = state.actions.map(i18n.action)
    .filter((action) => `${action.title} ${action.description} ${action.category}`.toLocaleLowerCase(i18n.getLocale()).includes(query))
    .map((action) => `<button class="command-result${action.available ? "" : " is-unavailable"}" type="button" data-command-action="${escapeHtml(action.id)}"${action.available ? "" : " disabled"}>${icon("run")}<span><strong>${escapeHtml(action.title)}</strong><span>${escapeHtml(action.available ? action.description : action.unavailableReason || t("operations.unavailable"))}</span></span><em>${escapeHtml(action.available ? riskLabel(action.risk) : t("operations.unavailable"))}</em></button>`);
  host.innerHTML = [...routeItems, ...actionItems].join("") || emptyRow(t("command.empty"));
  $$('[data-command-route]', host).forEach((button) => button.addEventListener("click", () => navigate(button.dataset.commandRoute)));
  $$('[data-command-action]', host).forEach((button) => button.addEventListener("click", () => { dismissDialog($("[data-command-dialog]")); openAction(button.dataset.commandAction); }));
}

function toast(title, detail, error = false) {
  const element = document.createElement("div");
  element.className = `toast${error ? " is-error" : ""}`;
  element.innerHTML = `<strong>${escapeHtml(title)}</strong><span>${escapeHtml(detail)}</span>`;
  $("[data-toasts]").append(element);
  window.setTimeout(() => element.remove(), 4200);
}

function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
  try { localStorage.setItem("rsetup-theme-v2", theme); } catch { /* local storage is optional */ }
}

function bindEvents() {
  $$('[data-route]').forEach((button) => button.addEventListener("click", () => navigate(button.dataset.route)));
  $$('[data-refresh]').forEach((button) => button.addEventListener("click", () => refreshAll()));
  $("[data-command-trigger]").addEventListener("click", openCommands);
  $("[data-command-input]").addEventListener("input", renderCommandResults);
  $("[data-command-dialog]").addEventListener("cancel", (event) => {
    event.preventDefault();
    dismissDialog(event.currentTarget);
  });
  $("[data-language-toggle]").addEventListener("click", () => i18n.setLocale(i18n.getLocale() === "zh-CN" ? "en" : "zh-CN"));
  $("[data-theme-toggle]").addEventListener("click", () => setTheme(document.documentElement.dataset.theme === "dark" ? "light" : "dark"));
  $("[data-task-close]").addEventListener("click", closeAction);
  $("[data-task-drawer]").addEventListener("cancel", (event) => { event.preventDefault(); closeAction(); });
  $("[data-task-confirm]").addEventListener("change", (event) => {
    $("[data-task-execute]").disabled = !state.selectedAction?.available || !event.currentTarget.checked;
  });
  $("[data-task-execute]").addEventListener("click", executeSelectedAction);
  $("[data-source-provider]").addEventListener("change", () => { clearSourcePlan(); renderProviderDetail(); });
  $("[data-source-preview]").addEventListener("click", previewSources);
  $("[data-source-confirm]").addEventListener("change", (event) => {
    $("[data-source-apply]").disabled = !event.currentTarget.checked || !state.sourcePlan?.changes.length;
  });
  $("[data-source-apply]").addEventListener("click", applySourcePlan);
  $("[data-debug-profile]").addEventListener("change", (event) => activateDebugProfile(event.currentTarget.value));
  $("[data-debug-apply]").addEventListener("click", applyCustomDebugDevice);
  $("[data-debug-reset]").addEventListener("click", () => activateDebugProfile("provider"));
  document.addEventListener("click", (event) => {
    const menu = $("[data-debug-menu]");
    if (menu.open && !menu.contains(event.target)) menu.open = false;
  });
  window.addEventListener("rsetup:locale", () => { applyStaticTranslations(); renderAll(); renderActionDetails(); });
  window.addEventListener("keydown", (event) => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); openCommands(); }
    if (event.key === "Escape" && $("[data-command-dialog]").open) {
      event.preventDefault();
      dismissDialog($("[data-command-dialog]"));
    } else if (event.key === "Escape" && $("[data-task-drawer]").classList.contains("is-open")) {
      closeAction();
    } else if (event.key === "Escape" && $("[data-debug-menu]").open) {
      $("[data-debug-menu]").open = false;
    }
  });
}

function startClock() {
  const tick = () => setText("[data-clock]", new Intl.DateTimeFormat(i18n.getLocale(), {
    hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
  }).format(new Date()));
  tick();
  window.setInterval(tick, 1000);
}

async function init() {
  loadDebugState();
  applyStaticTranslations();
  let storedTheme = null;
  try { storedTheme = localStorage.getItem("rsetup-theme-v2"); } catch { /* local storage is optional */ }
  setTheme(storedTheme || "light");
  bindEvents();
  startClock();
  const initialRoute = location.hash.slice(1);
  if (routes.some((route) => route.id === initialRoute)) navigate(initialRoute);
  await refreshAll({ quiet: true });
  window.setInterval(() => refreshAll({ quiet: true }), 10_000);
}

init();

const i18n = window.RsetupI18n;
const { t } = i18n;

const routes = [
  { id: "overview", icon: "overview" },
  { id: "system", icon: "system" },
  { id: "network", icon: "network" },
  { id: "hardware", icon: "chip" },
  { id: "workflows", icon: "run" },
  { id: "activity", icon: "pulse" },
  { id: "help", icon: "help" },
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
  led: { icon: "led", tone: "coral" },
  "spi-flash": { icon: "spi-flash", tone: "signal" },
};

const hardwareToolIds = new Set(["device-tree", "gpio", "video", "thermal", "led", "spi-flash"]);

const debugDeviceProfiles = [
  { id: "rockchip-rk3588", label: "ROCK 5B · RK3588", product: "Radxa ROCK 5B Demo", hostname: "debug-rock-5b", socVendor: "Rockchip", soc: "RK3588", architecture: "aarch64", pinoutProfile: "rock5b" },
  { id: "allwinner-a733", label: "Cubie A7A · A733", product: "Radxa Cubie A7A Demo", hostname: "debug-cubie-a7a", socVendor: "Allwinner", soc: "A733", architecture: "aarch64", pinoutProfile: "cubieA7a" },
  { id: "cix-p1", label: "Orion O6 · CIX P1", product: "Radxa Orion O6 Demo", hostname: "debug-orion-o6", socVendor: "CIX", soc: "P1", architecture: "aarch64", pinoutProfile: "orionO6" },
  { id: "qualcomm-qcs6490", label: "Dragon Q6A · QCS6490", product: "Radxa Dragon Q6A Demo", hostname: "debug-dragon-q6a", socVendor: "Qualcomm", soc: "QCS6490", architecture: "aarch64", pinoutProfile: "dragonQ6a" },
  { id: "amlogic-a311d", label: "ZERO 2 Pro · A311D", product: "Radxa ZERO 2 Pro Demo", hostname: "debug-zero-2-pro", socVendor: "Amlogic", soc: "A311D", architecture: "aarch64", pinoutProfile: "radxaZero2Pro" },
  { id: "mediatek-genio700", label: "MediaTek · Genio 700", product: "MediaTek Genio 700 Demo", hostname: "debug-genio700", socVendor: "MediaTek", soc: "Genio 700", architecture: "aarch64", pinoutProfile: null },
  { id: "starfive-jh7110", label: "StarFive · JH7110", product: "StarFive JH7110 Demo", hostname: "debug-jh7110", socVendor: "StarFive", soc: "JH7110", architecture: "riscv64", pinoutProfile: null },
];

const defaultDebugCustom = {
  product: "Custom SBC Demo",
  hostname: "debug-sbc",
  socVendor: "Rockchip",
  soc: "RK3588",
  architecture: "aarch64",
};

const helpProfiles = [
  {
    id: "rock-5b",
    name: "ROCK 5B",
    patterns: ["rock 5b", "rock5b"],
    resources: [
      { kind: "guide", path: "/rock5/rock5b" },
      { kind: "faq", path: "/rock5/rock5b/faq" },
      { kind: "download", path: "/rock5/rock5b/download" },
    ],
  },
  {
    id: "orion-o6",
    name: "Orion O6",
    patterns: ["orion o6", "orion-o6"],
    resources: [
      { kind: "guide", path: "/orion/o6" },
      { kind: "faq", path: "/orion/faq" },
      { kind: "download", path: "/orion/download" },
    ],
  },
  {
    id: "dragon-q6a",
    name: "Dragon Q6A",
    patterns: ["dragon q6a", "q6a"],
    resources: [
      { kind: "guide", path: "/dragon/q6a/getting-started" },
      { kind: "download", path: "/dragon/q6a/download" },
    ],
  },
  {
    id: "zero-2-pro",
    name: "ZERO 2 Pro",
    patterns: ["zero 2 pro", "zero2pro", "zero 2pro"],
    resources: [
      { kind: "guide", path: "/zero/zero2pro" },
      { kind: "download", path: "/zero/zero2pro/download" },
    ],
  },
];

const genericHelpResources = [
  { kind: "docs", path: "/welcome" },
  { kind: "forum", url: "https://forum.radxa.com/" },
  { kind: "github", url: "https://github.com/radxa" },
];

const helpFaqs = ["docs", "hardware", "privilege", "support"];

const contactChannels = [
  { id: "forum", mark: "FR", url: "https://forum.radxa.com/" },
  { id: "github", mark: "GH", url: "https://github.com/radxa" },
  { id: "discord", mark: "DC", url: "https://discord.com/invite/mn73YNWdHY" },
  { id: "telegram", mark: "TG", url: "https://t.me/rockpi4" },
];

const state = {
  providerSnapshot: null,
  snapshot: null,
  actions: [],
  activity: [],
  sources: null,
  sourcePlan: null,
  selectedAction: null,
  selectedHardware: null,
  hardwareData: null,
  hardwareLoadVersion: 0,
  gpioSelectedPin: null,
  overlayPlan: null,
  overlaySelection: [],
  videoFrame: null,
  thermalPolicy: null,
  thermalPanel: "policy",
  fanCurveDraft: null,
  fanCurvePlan: null,
  fanCurvePreviewVersion: 0,
  ledPanel: "status",
  ledSelection: null,
  rgbLedConfig: null,
  spiFlashOperation: "install",
  spiFlashTarget: null,
  spiFlashImage: null,
  spiFlashPlan: null,
  spiFlashPreviewVersion: 0,
  lastInvoker: null,
  contactInvoker: null,
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
  async overlayStatus() {
    if (tauriInvoke) return tauriInvoke("overlay_status");
    return request("/api/v1/hardware/overlays");
  },
  async planOverlays(selectedIds) {
    if (tauriInvoke) return tauriInvoke("plan_overlays", { selectedIds });
    return request("/api/v1/hardware/overlays/plan", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ selectedIds }),
    });
  },
  async applyOverlays(selectedIds, planToken, confirm) {
    if (tauriInvoke) return tauriInvoke("apply_overlays", { selectedIds, planToken, confirm });
    return request("/api/v1/hardware/overlays/apply", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ selectedIds, planToken, confirm }),
    });
  },
  async gpioStatus(profileId = null) {
    if (tauriInvoke) return tauriInvoke("gpio_status", { profileId });
    const query = profileId ? `?profile=${encodeURIComponent(profileId)}` : "";
    return request(`/api/v1/hardware/gpio${query}`);
  },
  async spiFlashStatus() {
    if (tauriInvoke) return tauriInvoke("spi_flash_status");
    return request("/api/v1/hardware/spi-flash");
  },
  async planSpiFlash(operation, targetId, imageId) {
    const requestBody = { operation, targetId, imageId };
    if (tauriInvoke) return tauriInvoke("plan_spi_flash", { request: requestBody });
    return request("/api/v1/hardware/spi-flash/plan", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(requestBody),
    });
  },
  async applySpiFlash(operation, targetId, imageId, planToken, confirm) {
    const requestBody = { operation, targetId, imageId };
    if (tauriInvoke) return tauriInvoke("apply_spi_flash", { request: requestBody, planToken, confirm });
    return request("/api/v1/hardware/spi-flash/apply", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ ...requestBody, planToken, confirm }),
    });
  },
  async videoStatus() {
    if (tauriInvoke) return tauriInvoke("video_status");
    return request("/api/v1/hardware/video");
  },
  async captureVideo(deviceId) {
    if (tauriInvoke) return tauriInvoke("capture_video_frame", { deviceId });
    return request("/api/v1/hardware/video/capture", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ deviceId }),
    });
  },
  async thermalStatus() {
    if (tauriInvoke) return tauriInvoke("thermal_status");
    return request("/api/v1/hardware/thermal");
  },
  async applyThermalPolicy(policy, confirm) {
    if (tauriInvoke) return tauriInvoke("apply_thermal_policy", { policy, confirm });
    return request("/api/v1/hardware/thermal/apply", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ policy, confirm }),
    });
  },
  async fanCurveStatus() {
    if (tauriInvoke) return tauriInvoke("fan_curve_status");
    return request("/api/v1/hardware/thermal/fan-curve");
  },
  async planFanCurve(requestBody) {
    if (tauriInvoke) return tauriInvoke("plan_fan_curve", { request: requestBody });
    return request("/api/v1/hardware/thermal/fan-curve/plan", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(requestBody),
    });
  },
  async applyFanCurve(requestBody, planToken, confirm) {
    if (tauriInvoke) return tauriInvoke("apply_fan_curve", { request: requestBody, planToken, confirm });
    return request("/api/v1/hardware/thermal/fan-curve/apply", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ ...requestBody, planToken, confirm }),
    });
  },
  async ledStatus() {
    if (tauriInvoke) return tauriInvoke("led_status");
    return request("/api/v1/hardware/leds");
  },
  async applyLedTrigger(ledId, trigger, confirm) {
    if (tauriInvoke) return tauriInvoke("apply_led_trigger", { ledId, trigger, confirm });
    return request("/api/v1/hardware/leds/trigger", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ ledId, trigger, confirm }),
    });
  },
  async applyRgbLed(config, confirm) {
    if (tauriInvoke) return tauriInvoke("apply_rgb_led", { config, confirm });
    return request("/api/v1/hardware/leds/rgb", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ config, confirm }),
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

function hardwareReason(reason) {
  if (i18n.getLocale() !== "zh-CN") return reason;
  if (reason === "No SPI NOR MTD device was detected.") return "未检测到 SPI NOR MTD 设备。";
  if (reason === "Install mtd-utils to write or erase SPI boot flash.") return "请安装 mtd-utils 后再写入或擦除 SPI 启动闪存。";
  if (reason === "No thermal zone with the user_space governor was detected.") return "未检测到支持 user_space 策略的温区。";
  if (reason === "No controllable pwm-fan cooling device was detected.") return "未检测到可控制的 pwm-fan 散热设备。";
  if (reason === "The detected thermal and fan controls are read-only.") return "检测到的温控与风扇接口为只读。";
  if (reason === "Install the rsetup-next fan curve service before enabling a curve.") return "请先安装 rsetup-next 风扇曲线服务。";
  return reason;
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

function helpProfile(identity) {
  const haystack = `${identity?.product || ""} ${identity?.hostname || ""}`.toLocaleLowerCase("en");
  return helpProfiles.find((profile) => profile.patterns.some((pattern) => haystack.includes(pattern))) || null;
}

function localizedDocsUrl(path) {
  const languagePath = i18n.getLocale() === "zh-CN" ? "" : "/en";
  return `https://docs.radxa.com${languagePath}${path}`;
}

function debugProfileById(id) {
  return debugDeviceProfiles.find((profile) => profile.id === id);
}

function gpioProfileOverride() {
  if (!state.snapshot?.synthetic || state.debugProfile === "provider") return null;
  if (state.debugProfile === "custom") return "none";
  return debugProfileById(state.debugProfile)?.pinoutProfile || "none";
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
  renderHelp();
  renderDebugControls();
  resolveSignals();
  if (state.selectedHardware === "gpio") void openHardwareTool("gpio");
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
  renderHelp();
  renderDebugControls();
  resolveSignals();
  if (state.selectedHardware === "gpio") void openHardwareTool("gpio");
  toast(t("debug.title"), t("debug.changed", { device: state.debugCustom.product }));
}

function applyStaticTranslations() {
  document.title = t("document.title");
  $("meta[name='description']")?.setAttribute("content", t("document.description"));
  $$('[data-i18n]').forEach((element) => { element.textContent = t(element.dataset.i18n); });
  $$('[data-i18n-aria]').forEach((element) => { element.setAttribute("aria-label", t(element.dataset.i18nAria)); });
  $$('[data-i18n-alt]').forEach((element) => { element.setAttribute("alt", t(element.dataset.i18nAlt)); });
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
  renderHelp();
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
    const interactive = hardwareToolIds.has(raw.id);
    const tag = interactive ? "button" : "article";
    const attributes = interactive
      ? `type="button" data-hardware-tool="${escapeHtml(raw.id)}"${capability.available ? "" : " disabled"}`
      : "";
    return `<${tag} class="hardware-cell ${interactive ? "is-interactive" : ""} ${capability.available ? "" : "is-offline"}" data-tone="${visual.tone}" ${attributes}>${icon(visual.icon)}<span class="hardware-cell-copy"><h2>${escapeHtml(capability.label)}</h2><p>${escapeHtml(capability.detail)}</p></span>${interactive ? `<span class="hardware-open">${escapeHtml(capability.available ? t("hardware.open") : t("operations.unavailable"))}${icon("run")}</span>` : ""}</${tag}>`;
  }).join("");
  bindHardwareButtons();
}

function bindHardwareButtons() {
  $$("[data-hardware-tool]").forEach((button) => {
    button.addEventListener("click", () => openHardwareTool(button.dataset.hardwareTool));
  });
}

function hardwareToolCopy(id) {
  const prefix = {
    "device-tree": "overlay",
    gpio: "gpio",
    video: "video",
    thermal: "thermal",
    led: "led",
    "spi-flash": "spiFlash",
  }[id] || "hardware";
  return { title: t(`${prefix}.title`), description: t(`${prefix}.description`) };
}

function overlayDisplayCopy(overlay) {
  if (!overlay) return {};
  if (!state.hardwareData?.synthetic || i18n.getLocale() !== "zh-CN") return overlay;
  const copies = {
    "rk3588-uart2-m0.dtbo": ["UART2 M0", "将 UART2 路由到 40 针排针。", "串口"],
    "rk3588-i2c3-m1.dtbo": ["I²C3 M1", "在扩展排针上启用 I²C3 总线。", "总线"],
    "rk3588-spi0-m2-cs0-spidev.dtbo": ["SPI0 M2", "通过 spidev 暴露 SPI0 片选 0。", "总线"],
    "rk3588-can1-m0.dtbo": ["CAN1 M0", "在扩展排针上启用 CAN1。", "现场总线"],
    "rk3588-pwm12-m0.dtbo": ["PWM12 M0", "暴露 PWM12，用于风扇或执行器控制。", "PWM"],
    "rk3588-disable-led.dtbo": ["关闭状态灯", "启动后关闭 SBC 状态灯。", "SBC"],
  };
  const copy = copies[overlay.id];
  return copy ? { ...overlay, title: copy[0], description: copy[1], category: copy[2] } : overlay;
}

async function openHardwareTool(id) {
  const capability = state.snapshot?.capabilities.find((item) => item.id === id);
  if (!hardwareToolIds.has(id) || !capability) return;
  if (!capability.available) {
    toast(i18n.capability(capability).label, t("hardware.unavailable"), true);
    return;
  }
  state.selectedHardware = id;
  const loadVersion = ++state.hardwareLoadVersion;
  state.hardwareData = null;
  state.gpioSelectedPin = null;
  state.overlayPlan = null;
  state.overlaySelection = [];
  state.videoFrame = null;
  state.thermalPolicy = null;
  state.thermalPanel = "policy";
  state.fanCurveDraft = null;
  resetFanCurvePlan();
  state.ledPanel = "status";
  state.ledSelection = null;
  state.rgbLedConfig = null;
  state.spiFlashOperation = "install";
  state.spiFlashTarget = null;
  state.spiFlashImage = null;
  resetSpiFlashPlan();
  state.lastInvoker = document.activeElement;
  const copy = hardwareToolCopy(id);
  setText("[data-hardware-tool-title]", copy.title);
  setText("[data-hardware-tool-description]", copy.description);
  $("[data-hardware-body]").innerHTML = `<div class="hardware-tool-loading"><span></span><span>${escapeHtml(t("hardware.loading"))}</span></div>`;
  const drawer = $("[data-hardware-drawer]");
  if (!drawer.open) openDialog(drawer);
  requestAnimationFrame(() => {
    drawer.classList.add("is-open");
    $("[data-hardware-close]").focus();
  });
  try {
    const loaders = {
      "device-tree": () => transport.overlayStatus(),
      gpio: () => transport.gpioStatus(gpioProfileOverride()),
      video: () => transport.videoStatus(),
      thermal: async () => {
        const [thermal, fanCurve] = await Promise.all([transport.thermalStatus(), transport.fanCurveStatus()]);
        return { ...thermal, fanCurve };
      },
      led: () => transport.ledStatus(),
      "spi-flash": () => transport.spiFlashStatus(),
    };
    const data = await loaders[id]();
    if (state.selectedHardware !== id || state.hardwareLoadVersion !== loadVersion) return;
    state.hardwareData = data;
    if (id === "device-tree") state.overlaySelection = data.overlays.filter((item) => item.enabled).map((item) => item.id);
    if (id === "thermal") {
      state.thermalPolicy = data.currentPolicy || data.recommendedPolicy || data.availablePolicies[0] || null;
      initializeFanCurveState(data.fanCurve);
      state.thermalPanel = data.fanCurve?.config ? "curve" : "policy";
    }
    if (id === "led") initializeLedState(data);
    if (id === "spi-flash") initializeSpiFlashState(data);
    renderHardwareTool();
  } catch (error) {
    if (state.selectedHardware !== id || state.hardwareLoadVersion !== loadVersion) return;
    renderHardwareError(displayError(error));
  }
}

function closeHardwareTool() {
  const drawer = $("[data-hardware-drawer]");
  drawer.classList.remove("is-open");
  window.setTimeout(() => {
    if (drawer.open) dismissDialog(drawer);
    state.lastInvoker?.focus?.();
    state.lastInvoker = null;
  }, 320);
  state.selectedHardware = null;
  state.hardwareLoadVersion += 1;
  state.hardwareData = null;
  state.overlayPlan = null;
  resetSpiFlashPlan();
  resetFanCurvePlan();
}

function renderHardwareError(message) {
  $("[data-hardware-body]").innerHTML = `<div class="hardware-tool-error">${icon("pulse")}<strong>${escapeHtml(t("toast.failed"))}</strong><span>${escapeHtml(message)}</span></div>`;
}

function renderHardwareTool() {
  if (!state.selectedHardware || !state.hardwareData) return;
  const copy = hardwareToolCopy(state.selectedHardware);
  setText("[data-hardware-tool-title]", copy.title);
  setText("[data-hardware-tool-description]", copy.description);
  if (state.selectedHardware === "device-tree") renderOverlayTool();
  else if (state.selectedHardware === "gpio") renderGpioTool();
  else if (state.selectedHardware === "video") renderVideoTool();
  else if (state.selectedHardware === "thermal") renderThermalTool();
  else if (state.selectedHardware === "led") renderLedTool();
  else if (state.selectedHardware === "spi-flash") renderSpiFlashTool();
}

function renderOverlayTool() {
  const data = state.hardwareData;
  const selected = new Set(state.overlaySelection);
  const host = $("[data-hardware-body]");
  if (!data.supported || !data.overlays.length) {
    host.innerHTML = `<div class="hardware-tool-empty">${escapeHtml(data.unavailableReason || t("overlay.none"))}</div>`;
    return;
  }
  host.innerHTML = `
    <div class="tool-fact-line"><span>${escapeHtml(t("overlay.location", { bootloader: data.bootloader, directory: data.directory || "—" }))}</span><b>${data.overlays.filter((item) => item.enabled).length}/${data.overlays.length}</b></div>
    <div class="overlay-list">
      ${data.overlays.map((rawOverlay) => {
        const overlay = overlayDisplayCopy(rawOverlay);
        return `<label class="overlay-row">
        <input type="checkbox" value="${escapeHtml(overlay.id)}" ${selected.has(overlay.id) ? "checked" : ""} ${data.mutable ? "" : "disabled"} />
        <span class="overlay-switch" aria-hidden="true"></span>
        <span class="overlay-copy"><strong>${escapeHtml(overlay.title)}</strong><span>${escapeHtml(overlay.description || overlay.id)}</span><small>${escapeHtml([overlay.category, overlay.id].filter(Boolean).join(" · "))}</small></span>
        <b>${escapeHtml(selected.has(overlay.id) ? t("overlay.enabled") : t("overlay.disabled"))}</b>
      </label>`;
      }).join("")}
    </div>
    <button class="secondary-button overlay-preview" type="button" data-overlay-preview ${data.mutable ? "" : "disabled"}><span>${escapeHtml(t("overlay.preview"))}</span>${icon("run")}</button>
    <div data-overlay-plan></div>`;
  $$('input[type="checkbox"]', host).forEach((input) => input.addEventListener("change", () => {
    state.overlaySelection = $$('input[type="checkbox"]', host).filter((item) => item.checked).map((item) => item.value);
    state.overlayPlan = null;
    renderOverlayTool();
  }));
  $("[data-overlay-preview]", host)?.addEventListener("click", previewOverlays);
  if (!data.mutable && data.unavailableReason) {
    $("[data-overlay-plan]", host).innerHTML = `<div class="tool-warning">${escapeHtml(data.unavailableReason)}</div>`;
  } else if (state.overlayPlan) {
    renderOverlayPlan();
  }
}

async function previewOverlays() {
  const button = $("[data-overlay-preview]");
  button.disabled = true;
  $("span", button).textContent = t("overlay.previewing");
  try {
    state.overlayPlan = await transport.planOverlays(state.overlaySelection);
    renderOverlayTool();
  } catch (error) {
    renderHardwareError(displayError(error));
  }
}

function renderOverlayPlan() {
  const plan = state.overlayPlan;
  const host = $("[data-overlay-plan]");
  if (!host || !plan) return;
  host.innerHTML = `<section class="hardware-plan">
    <div class="hardware-plan-head"><strong>${escapeHtml(t("overlay.changeCount", { count: plan.changes.length }))}</strong><span>${escapeHtml(plan.synthetic ? t("activity.simulated") : t("drawer.root"))}</span></div>
    ${plan.changes.length ? `<ul>${plan.changes.map((change) => {
      const overlay = overlayDisplayCopy(state.hardwareData.overlays.find((item) => item.id === change.id));
      return `<li><span>${escapeHtml(change.afterEnabled ? t("overlay.enable", { name: overlay?.title || change.id }) : t("overlay.disable", { name: overlay?.title || change.id }))}</span></li>`;
    }).join("")}</ul>` : `<p>${escapeHtml(t("overlay.noChanges"))}</p>`}
    <div class="tool-warning"><span>${escapeHtml(t("overlay.warning.reboot"))}</span><span>${escapeHtml(t("overlay.warning.kernel"))}</span></div>
    <label class="confirm-line hardware-confirm"><input type="checkbox" data-overlay-confirm ${plan.changes.length ? "" : "disabled"} /><span>${escapeHtml(t("overlay.confirm"))}</span></label>
    <button class="execute-button hardware-execute" type="button" data-overlay-apply disabled><span>${escapeHtml(t("overlay.apply"))}</span>${icon("run")}</button>
    <div class="drawer-result" data-overlay-result hidden></div>
  </section>`;
  $("[data-overlay-confirm]", host).addEventListener("change", (event) => {
    $("[data-overlay-apply]", host).disabled = !event.currentTarget.checked || !plan.changes.length;
  });
  $("[data-overlay-apply]", host).addEventListener("click", applyOverlays);
}

async function applyOverlays() {
  const plan = state.overlayPlan;
  if (!plan) return;
  const button = $("[data-overlay-apply]");
  const result = $("[data-overlay-result]");
  button.disabled = true;
  $("span", button).textContent = t("overlay.applying");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("drawer.runState");
  try {
    const applied = await transport.applyOverlays(state.overlaySelection, plan.planToken, true);
    result.textContent = applied.run.synthetic ? t("sources.planned") : t("overlay.saved");
    toast(applied.run.synthetic ? t("toast.dryRun") : t("overlay.saved"), t("overlay.warning.reboot"));
    if (!applied.run.synthetic) {
      state.hardwareData = await transport.overlayStatus();
      state.overlaySelection = state.hardwareData.overlays.filter((item) => item.enabled).map((item) => item.id);
      state.overlayPlan = null;
      renderOverlayTool();
    }
    await refreshAll({ quiet: true });
  } catch (error) {
    const detail = displayError(error);
    result.classList.add("is-error");
    result.textContent = detail;
    $("span", button).textContent = t("overlay.apply");
    button.disabled = false;
  }
}

function initializeSpiFlashState(data) {
  state.spiFlashOperation = data.images?.length ? "install" : "erase";
  state.spiFlashTarget = data.devices?.[0]?.id || null;
  state.spiFlashImage = data.images?.[0]?.id || null;
  resetSpiFlashPlan();
}

function spiFlashRequest() {
  return {
    operation: state.spiFlashOperation,
    targetId: state.spiFlashTarget,
    imageId: state.spiFlashOperation === "install" ? state.spiFlashImage : null,
  };
}

function sameSpiFlashRequest(left, right) {
  return left?.operation === right?.operation
    && left?.targetId === right?.targetId
    && (left?.imageId || null) === (right?.imageId || null);
}

function resetSpiFlashPlan() {
  state.spiFlashPlan = null;
  state.spiFlashPreviewVersion += 1;
}

function renderSpiFlashTool() {
  const data = state.hardwareData;
  const host = $("[data-hardware-body]");
  if (!data.supported || !data.devices.length) {
    host.innerHTML = `<div class="hardware-tool-empty">${escapeHtml(hardwareReason(data.unavailableReason) || t("spiFlash.none"))}</div>`;
    return;
  }
  const target = data.devices.find((device) => device.id === state.spiFlashTarget) || data.devices[0];
  const image = data.images.find((item) => item.id === state.spiFlashImage) || data.images[0] || null;
  state.spiFlashTarget = target.id;
  state.spiFlashImage = image?.id || null;
  const canPreview = data.mutable && (state.spiFlashOperation === "erase" || Boolean(image));
  host.innerHTML = `
    <div class="tool-fact-line"><span>${escapeHtml(t("spiFlash.detected", { devices: data.devices.length, images: data.images.length }))}</span><b>SPI NOR</b></div>
    <div class="spi-operation-tabs" role="tablist" aria-label="${escapeHtml(t("spiFlash.operation"))}">
      <button type="button" role="tab" data-spi-operation="install" aria-selected="${state.spiFlashOperation === "install"}" ${data.images.length ? "" : "disabled"}>${icon("spi-flash")}<span>${escapeHtml(t("spiFlash.install"))}</span></button>
      <button type="button" role="tab" data-spi-operation="erase" aria-selected="${state.spiFlashOperation === "erase"}">${icon("close")}<span>${escapeHtml(t("spiFlash.erase"))}</span></button>
    </div>
    <div class="spi-flash-fields">
      <label class="tool-field"><span>${escapeHtml(t("spiFlash.target"))}</span><select data-spi-target ${data.mutable ? "" : "disabled"}>${data.devices.map((device) => `<option value="${escapeHtml(device.id)}" ${device.id === target.id ? "selected" : ""}>${escapeHtml(device.name)} · ${escapeHtml(device.path)}</option>`).join("")}</select></label>
      ${state.spiFlashOperation === "install" ? `<label class="tool-field"><span>${escapeHtml(t("spiFlash.image"))}</span><select data-spi-image ${data.mutable && data.images.length ? "" : "disabled"}>${data.images.length ? data.images.map((item) => `<option value="${escapeHtml(item.id)}" ${item.id === image?.id ? "selected" : ""}>${escapeHtml(item.title)} · ${escapeHtml(item.layout)}</option>`).join("") : `<option>${escapeHtml(t("spiFlash.noImages"))}</option>`}</select></label>` : ""}
    </div>
    <div class="spi-device-facts">
      <span><i>${escapeHtml(t("spiFlash.device"))}</i><b>${escapeHtml(target.path)}</b></span>
      <span><i>${escapeHtml(t("spiFlash.capacity"))}</i><b>${escapeHtml(byteUnit(target.sizeBytes))}</b></span>
      <span><i>${escapeHtml(t("spiFlash.eraseBlock"))}</i><b>${escapeHtml(byteUnit(target.eraseSizeBytes))}</b></span>
    </div>
    ${image && state.spiFlashOperation === "install" ? `<section class="spi-image-layout"><div><strong>${escapeHtml(image.title)}</strong><span>${escapeHtml(t("spiFlash.imageSize", { size: byteUnit(image.sizeBytes) }))}</span></div><ol>${image.components.map((component) => `<li><span>${escapeHtml(component.fileName)}</span><b>${escapeHtml(t("spiFlash.offset", { offset: byteUnit(component.offsetBytes) }))}</b></li>`).join("")}</ol></section>` : ""}
    ${!data.mutable && data.unavailableReason ? `<div class="tool-warning">${escapeHtml(hardwareReason(data.unavailableReason))}</div>` : ""}
    ${state.spiFlashOperation === "erase" ? `<div class="tool-warning">${escapeHtml(t("spiFlash.eraseWarning"))}</div>` : ""}
    <button class="secondary-button spi-preview" type="button" data-spi-preview ${canPreview ? "" : "disabled"}><span>${escapeHtml(t("spiFlash.preview"))}</span>${icon("run")}</button>
    <div data-spi-plan></div>`;
  $$('[data-spi-operation]', host).forEach((button) => button.addEventListener("click", () => {
    state.spiFlashOperation = button.dataset.spiOperation;
    resetSpiFlashPlan();
    renderSpiFlashTool();
  }));
  $("[data-spi-target]", host)?.addEventListener("change", (event) => {
    state.spiFlashTarget = event.currentTarget.value;
    resetSpiFlashPlan();
    renderSpiFlashTool();
  });
  $("[data-spi-image]", host)?.addEventListener("change", (event) => {
    state.spiFlashImage = event.currentTarget.value;
    resetSpiFlashPlan();
    renderSpiFlashTool();
  });
  $("[data-spi-preview]", host)?.addEventListener("click", previewSpiFlash);
  if (state.spiFlashPlan) renderSpiFlashPlan();
}

async function previewSpiFlash() {
  const requestBody = spiFlashRequest();
  if (!requestBody.targetId || (requestBody.operation === "install" && !requestBody.imageId)) return;
  const previewVersion = state.spiFlashPreviewVersion + 1;
  state.spiFlashPreviewVersion = previewVersion;
  const button = $("[data-spi-preview]");
  button.disabled = true;
  $("span", button).textContent = t("spiFlash.previewing");
  try {
    const plan = await transport.planSpiFlash(requestBody.operation, requestBody.targetId, requestBody.imageId);
    if (previewVersion !== state.spiFlashPreviewVersion || state.selectedHardware !== "spi-flash") return;
    if (!sameSpiFlashRequest(spiFlashRequest(), requestBody) || !sameSpiFlashRequest(plan.request, requestBody)) {
      state.spiFlashPlan = null;
      renderSpiFlashTool();
      toast(t("toast.failed"), t("api.stale_plan"), true);
      return;
    }
    state.spiFlashPlan = plan;
    renderSpiFlashTool();
  } catch (error) {
    if (previewVersion !== state.spiFlashPreviewVersion || state.selectedHardware !== "spi-flash") return;
    renderHardwareError(displayError(error));
  }
}

function renderSpiFlashPlan() {
  const plan = state.spiFlashPlan;
  const host = $("[data-spi-plan]");
  if (!host || !plan) return;
  const operation = plan.request.operation;
  const imageName = plan.image?.title || "—";
  const confirmCopy = operation === "install"
    ? t("spiFlash.confirmInstall", { target: plan.target.path, image: imageName })
    : t("spiFlash.confirmErase", { target: plan.target.path });
  host.innerHTML = `<section class="hardware-plan spi-plan">
    <div class="hardware-plan-head"><strong>${escapeHtml(t(`spiFlash.plan.${operation}`))}</strong><span>${escapeHtml(plan.synthetic ? t("activity.simulated") : t("drawer.root"))}</span></div>
    <dl><div><dt>${escapeHtml(t("spiFlash.target"))}</dt><dd>${escapeHtml(plan.target.path)}</dd></div>${plan.image ? `<div><dt>${escapeHtml(t("spiFlash.image"))}</dt><dd>${escapeHtml(plan.image.title)}</dd></div>` : ""}<div><dt>${escapeHtml(t("spiFlash.backup"))}</dt><dd>${escapeHtml(t("spiFlash.backupBefore"))}</dd></div><div><dt>${escapeHtml(t("spiFlash.verify"))}</dt><dd>${escapeHtml(t("spiFlash.readback"))}</dd></div></dl>
    <div class="tool-warning"><span>${escapeHtml(t("spiFlash.powerWarning"))}</span><span>${escapeHtml(t("spiFlash.bootWarning"))}</span></div>
    <label class="confirm-line hardware-confirm"><input type="checkbox" data-spi-confirm /><span>${escapeHtml(confirmCopy)}</span></label>
    <button class="execute-button hardware-execute" type="button" data-spi-apply disabled><span>${escapeHtml(t(`spiFlash.apply.${operation}`))}</span>${icon("run")}</button>
    <div class="drawer-result" data-spi-result hidden></div>
  </section>`;
  $("[data-spi-confirm]", host).addEventListener("change", (event) => {
    $("[data-spi-apply]", host).disabled = !event.currentTarget.checked;
  });
  $("[data-spi-apply]", host).addEventListener("click", applySpiFlash);
}

async function applySpiFlash() {
  const plan = state.spiFlashPlan;
  if (!plan) return;
  if (!sameSpiFlashRequest(spiFlashRequest(), plan.request)) {
    resetSpiFlashPlan();
    renderSpiFlashTool();
    toast(t("toast.failed"), t("api.stale_plan"), true);
    return;
  }
  const button = $("[data-spi-apply]");
  const result = $("[data-spi-result]");
  button.disabled = true;
  $("span", button).textContent = t("spiFlash.applying");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("drawer.runState");
  try {
    const requestBody = plan.request;
    const applied = await transport.applySpiFlash(requestBody.operation, requestBody.targetId, requestBody.imageId, plan.planToken, true);
    const message = applied.run.synthetic ? t("sources.planned") : t(`spiFlash.applied.${requestBody.operation}`);
    result.textContent = applied.backupPath ? `${message} · ${t("spiFlash.backupPath", { path: applied.backupPath })}` : message;
    toast(applied.run.synthetic ? t("toast.dryRun") : message, plan.target.path);
    $("span", button).textContent = t(`spiFlash.apply.${requestBody.operation}`);
    const confirmation = $("[data-spi-confirm]");
    if (confirmation) confirmation.checked = false;
    if (!applied.run.synthetic) {
      state.hardwareData = await transport.spiFlashStatus();
      initializeSpiFlashState(state.hardwareData);
      renderSpiFlashTool();
    }
    await refreshAll({ quiet: true });
  } catch (error) {
    const detail = displayError(error);
    const confirmation = $("[data-spi-confirm]");
    if (confirmation) confirmation.checked = false;
    if (error?.code === "stale_plan" || error?.code === "plan_required") {
      resetSpiFlashPlan();
      renderSpiFlashTool();
      toast(t("toast.failed"), detail, true);
      return;
    }
    result.classList.add("is-error");
    result.textContent = detail;
    $("span", button).textContent = t(`spiFlash.apply.${plan.request.operation}`);
    button.disabled = true;
  }
}

function gpioFunction(pin) {
  if (pin.currentFunction) return pin.currentFunction;
  if (pin.functionSource === "conflict") return t("gpio.conflict");
  return t("gpio.unassigned");
}

function gpioSource(pin) {
  return t(`gpio.source.${pin.functionSource || "unassigned"}`);
}

function gpioPinCell(pin, selected) {
  if (!pin) return `<span class="gpio-pin is-missing" aria-hidden="true"></span>`;
  const title = `${t("gpio.pin", { pin: pin.physicalPin })} · ${gpioFunction(pin)} · ${gpioSource(pin)}`;
  const pad = pin.functionSource === "default" || gpioFunction(pin) === pin.label ? "" : `<small>${escapeHtml(pin.label)}</small>`;
  return `<button class="gpio-pin ${selected ? "is-selected" : ""}" type="button" data-gpio-pin="${pin.physicalPin}" data-kind="${escapeHtml(pin.kind)}" data-function="${escapeHtml(pin.functionKind || "unassigned")}" data-source="${escapeHtml(pin.functionSource || "unassigned")}" aria-pressed="${selected}" title="${escapeHtml(title)}"><b>${pin.physicalPin}</b><span><strong>${escapeHtml(gpioFunction(pin))}</strong>${pad}</span><i>${escapeHtml(gpioSource(pin))}</i></button>`;
}

function gpioPinDetail(pin) {
  const identity = pin.label;
  const pad = pin.functionSource === "default" ? "" : `<div><dt>${escapeHtml(t("gpio.pad"))}</dt><dd>${escapeHtml(identity)}</dd></div>`;
  const source = pin.functionSource === "default" ? t("gpio.function1") : pin.sourceDetail || gpioSource(pin);
  return `<section class="gpio-pin-detail" data-source="${escapeHtml(pin.functionSource || "unassigned")}" aria-live="polite">
    <header><span>${escapeHtml(t("gpio.pin", { pin: pin.physicalPin }))}</span><strong>${escapeHtml(gpioFunction(pin))}</strong><i>${escapeHtml(gpioSource(pin))}</i></header>
    <dl>
      ${pad}
      <div><dt>${escapeHtml(t("gpio.configuration"))}</dt><dd>${escapeHtml(source)}</dd></div>
    </dl>
  </section>`;
}

function gpioConnectorView(connector, pins, selectedPin, index) {
  const numbers = connector.pinNumbers || [];
  const label = connector.id === "main" ? t("gpio.header.main") : t("gpio.header.connector", { index: index + 1 });
  return `<section class="gpio-connector">
    <header><strong>${escapeHtml(label)}</strong><span>${escapeHtml(t("gpio.pinCount", { count: numbers.length }))}</span></header>
    <div class="gpio-header" aria-label="${escapeHtml(label)}">
      ${Array.from({ length: Math.ceil(numbers.length / 2) }, (_, index) => {
        const left = pins.get(numbers[index * 2]);
        const right = pins.get(numbers[index * 2 + 1]);
        return `<div class="gpio-pair">${gpioPinCell(left, left?.physicalPin === selectedPin)}${gpioPinCell(right, right?.physicalPin === selectedPin)}</div>`;
      }).join("")}
    </div>
  </section>`;
}

function renderGpioTool() {
  const data = state.hardwareData;
  const host = $("[data-hardware-body]");
  if (!data.supported && !data.fanCurve?.supported) {
    host.innerHTML = `<div class="hardware-tool-empty">${escapeHtml(data.unavailableReason || t("hardware.unavailable"))}</div>`;
    return;
  }
  const pins = new Map(data.pins.map((pin) => [pin.physicalPin, pin]));
  const selectedPin = pins.has(state.gpioSelectedPin)
    ? state.gpioSelectedPin
    : (data.pins.find((pin) => pin.functionSource === "overlay") || data.pins.find((pin) => pin.functionSource === "conflict") || data.pins[0])?.physicalPin;
  state.gpioSelectedPin = selectedPin;
  const selected = pins.get(selectedPin);
  const connectors = data.connectors?.length ? data.connectors : [{ id: "main", label: "40-Pin GPIO Header", pinNumbers: data.pins.map((pin) => pin.physicalPin) }];
  host.innerHTML = `
    <div class="gpio-profile-card">
      <div class="gpio-profile-title"><strong>${escapeHtml(data.boardName || t("gpio.genericHeader"))}</strong><span>${escapeHtml(t(data.profileId ? "gpio.profileMatched" : "gpio.profileFallback"))}</span></div>
      <p>${escapeHtml(t(data.profileId ? "gpio.profileDescription" : "gpio.genericDescription"))}</p>
      <div class="gpio-profile-facts"><b>${escapeHtml(t("gpio.overlays", { count: data.configuredOverlays?.length || 0 }))}</b><b>${escapeHtml(data.layout || "40-pin")}</b></div>
    </div>
    ${data.serialConsoleDetected ? `<div class="tool-warning">${escapeHtml(t("gpio.serialWarning"))}</div>` : ""}
    <div class="gpio-view-note">${escapeHtml(t("gpio.currentOnly"))}</div>
    ${selected ? gpioPinDetail(selected) : ""}
    <div class="gpio-connectors">${connectors.map((connector, index) => gpioConnectorView(connector, pins, selectedPin, index)).join("")}</div>`;
  $$('[data-gpio-pin]', host).forEach((button) => {
    button.addEventListener("click", () => {
      state.gpioSelectedPin = Number(button.dataset.gpioPin);
      renderGpioTool();
      requestAnimationFrame(() => $(`[data-gpio-pin="${state.gpioSelectedPin}"]`, host)?.focus());
    });
  });
}

function renderVideoTool() {
  const data = state.hardwareData;
  const host = $("[data-hardware-body]");
  if (!data.supported) {
    host.innerHTML = `<div class="hardware-tool-empty">${escapeHtml(data.unavailableReason || t("hardware.unavailable"))}</div>`;
    return;
  }
  const current = $("[data-video-device]", host)?.value || data.devices[0]?.id;
  const frame = state.videoFrame;
  host.innerHTML = `
    <label class="tool-field"><span>${escapeHtml(t("video.device"))}</span><select data-video-device>${data.devices.map((device) => `<option value="${escapeHtml(device.id)}" ${device.id === current ? "selected" : ""}>${escapeHtml(device.name)} · ${escapeHtml(device.path)}</option>`).join("")}</select></label>
    <div class="camera-stage" data-camera-stage>
      ${frame ? `<img src="data:${escapeHtml(frame.mimeType)};base64,${frame.base64}" alt="${escapeHtml(t("video.title"))}" /><span>${escapeHtml(frame.synthetic ? t("video.synthetic") : t("video.captured", { time: relativeTime(frame.capturedAt) }))}</span>` : `<div>${icon("video")}<span>${escapeHtml(t("video.ready"))}</span></div>`}
    </div>
    ${!data.captureAvailable && data.unavailableReason ? `<div class="tool-warning">${escapeHtml(data.unavailableReason)}</div>` : ""}
    <button class="execute-button hardware-execute" type="button" data-video-capture ${data.captureAvailable ? "" : "disabled"}><span>${escapeHtml(t("video.capture"))}</span>${icon("video")}</button>`;
  $("[data-video-device]", host).addEventListener("change", () => { state.videoFrame = null; renderVideoTool(); });
  $("[data-video-capture]", host).addEventListener("click", captureVideoFrame);
}

async function captureVideoFrame() {
  const deviceId = $("[data-video-device]").value;
  const button = $("[data-video-capture]");
  button.disabled = true;
  $("span", button).textContent = t("video.capturing");
  try {
    state.videoFrame = await transport.captureVideo(deviceId);
    renderVideoTool();
  } catch (error) {
    renderHardwareError(displayError(error));
  }
}

function defaultFanCurveConfig(status) {
  const zone = status?.zones?.find((item) => item.supportsUserSpace) || status?.zones?.[0];
  const device = status?.coolingDevices?.[0];
  return {
    zoneId: zone?.id || "",
    coolingDeviceId: device?.id || "",
    pollIntervalMs: 2000,
    hysteresisC: 2,
    points: [
      { temperatureC: 40, speedPercent: 20 },
      { temperatureC: 55, speedPercent: 45 },
      { temperatureC: 70, speedPercent: 75 },
      { temperatureC: 82, speedPercent: 100 },
    ],
  };
}

function initializeFanCurveState(status) {
  const source = status?.config || defaultFanCurveConfig(status);
  state.fanCurveDraft = { ...source, points: (source.points || []).map((point) => ({ ...point })) };
  resetFanCurvePlan();
}

function resetFanCurvePlan() {
  state.fanCurvePlan = null;
  state.fanCurvePreviewVersion += 1;
}

function fanCurveRequest() {
  const draft = state.fanCurveDraft;
  return {
    enabled: true,
    config: {
      zoneId: draft?.zoneId || "",
      coolingDeviceId: draft?.coolingDeviceId || "",
      pollIntervalMs: Number(draft?.pollIntervalMs),
      hysteresisC: Number(draft?.hysteresisC),
      points: (draft?.points || []).map((point) => ({ temperatureC: Number(point.temperatureC), speedPercent: Number(point.speedPercent) })),
    },
  };
}

function normalizedFanCurveRequest(request) {
  if (!request?.enabled) return { enabled: false, config: null };
  return {
    enabled: true,
    config: {
      zoneId: request.config?.zoneId || "",
      coolingDeviceId: request.config?.coolingDeviceId || "",
      pollIntervalMs: Number(request.config?.pollIntervalMs),
      hysteresisC: Number(request.config?.hysteresisC),
      points: (request.config?.points || []).map((point) => [Number(point.temperatureC), Number(point.speedPercent)]),
    },
  };
}

function sameFanCurveRequest(left, right) {
  return JSON.stringify(normalizedFanCurveRequest(left)) === JSON.stringify(normalizedFanCurveRequest(right));
}

function fanCurveValidationKey(request) {
  if (!request.enabled) return null;
  const config = request.config;
  if (!config.zoneId || !config.coolingDeviceId) return "fanCurve.error.target";
  if (!Number.isFinite(config.hysteresisC) || config.hysteresisC < 0 || config.hysteresisC > 10) return "fanCurve.error.hysteresis";
  if (!Number.isInteger(config.pollIntervalMs) || config.pollIntervalMs < 500 || config.pollIntervalMs > 10000) return "fanCurve.error.poll";
  if (config.points.length < 2 || config.points.length > 8) return "fanCurve.error.count";
  let previousTemperature = -Infinity;
  let previousSpeed = 0;
  for (const point of config.points) {
    if (!Number.isFinite(point.temperatureC) || point.temperatureC < 0 || point.temperatureC > 110 || !Number.isInteger(point.speedPercent) || point.speedPercent < 0 || point.speedPercent > 100) return "fanCurve.error.range";
    if (point.temperatureC <= previousTemperature || point.speedPercent < previousSpeed) return "fanCurve.error.order";
    previousTemperature = point.temperatureC;
    previousSpeed = point.speedPercent;
  }
  const last = config.points.at(-1);
  if (last.speedPercent !== 100 || last.temperatureC > 90) return "fanCurve.error.maximum";
  return null;
}

function fanCurveChart(points, currentTemperature) {
  const clean = (points || []).filter((point) => Number.isFinite(Number(point.temperatureC)) && Number.isFinite(Number(point.speedPercent)));
  const minimum = Math.min(20, ...clean.map((point) => Number(point.temperatureC)));
  const maximum = Math.max(90, ...clean.map((point) => Number(point.temperatureC)));
  const midpoint = minimum + (maximum - minimum) / 2;
  const x = (temperature) => ((Number(temperature) - minimum) / Math.max(1, maximum - minimum)) * 100;
  const y = (speed) => 100 - Math.max(0, Math.min(100, Number(speed)));
  const path = clean.map((point) => `${x(point.temperatureC).toFixed(1)},${y(point.speedPercent).toFixed(1)}`).join(" ");
  const boundedTemperature = Math.max(minimum, Math.min(maximum, Number(currentTemperature)));
  const marker = Number.isFinite(Number(currentTemperature))
    ? `<line class="fan-curve-temperature" vector-effect="non-scaling-stroke" x1="${x(boundedTemperature).toFixed(1)}" x2="${x(boundedTemperature).toFixed(1)}" y1="0" y2="100"></line>`
    : "";
  return `<div class="fan-curve-plot">
    <svg viewBox="0 0 100 100" preserveAspectRatio="none" role="img" aria-label="${escapeHtml(t("fanCurve.chart"))}">
      <path class="fan-curve-grid" vector-effect="non-scaling-stroke" d="M0 0H100M0 50H100M0 100H100"></path>
      <path class="fan-curve-axes" vector-effect="non-scaling-stroke" d="M0 0V100H100"></path>
      ${marker}<polyline class="fan-curve-line" vector-effect="non-scaling-stroke" points="${path}"></polyline>
    </svg>
    ${clean.map((point) => `<i class="fan-curve-dot" style="--fan-x:${x(point.temperatureC).toFixed(1)}%;--fan-y:${y(point.speedPercent).toFixed(1)}%"></i>`).join("")}
  </div>
  <span class="fan-curve-axis fan-curve-axis-y is-top" aria-hidden="true">100%</span>
  <span class="fan-curve-axis fan-curve-axis-y is-middle" aria-hidden="true">50%</span>
  <span class="fan-curve-axis fan-curve-axis-y is-bottom" aria-hidden="true">0%</span>
  <span class="fan-curve-axis fan-curve-axis-x is-start" aria-hidden="true">${formatNumber(minimum, 0)}°</span>
  <span class="fan-curve-axis fan-curve-axis-x is-middle" aria-hidden="true">${formatNumber(midpoint, 0)}°</span>
  <span class="fan-curve-axis fan-curve-axis-x is-end" aria-hidden="true">${formatNumber(maximum, 0)}°</span>
  <span class="fan-curve-axis-title is-y" aria-hidden="true">${escapeHtml(t("fanCurve.speed"))} / %</span>
  <span class="fan-curve-axis-title is-x" aria-hidden="true">${escapeHtml(t("fanCurve.temperature"))} / °C</span>`;
}

function renderThermalTool() {
  const data = state.hardwareData;
  const host = $("[data-hardware-body]");
  if (!data.supported) {
    host.innerHTML = `<div class="hardware-tool-empty">${escapeHtml(data.unavailableReason || t("hardware.unavailable"))}</div>`;
    return;
  }
  host.innerHTML = `
    <div class="thermal-tabs led-tabs" role="tablist" aria-label="${escapeHtml(t("thermal.title"))}">
      <button type="button" role="tab" data-thermal-panel="policy" aria-selected="${state.thermalPanel === "policy"}">${icon("thermal")}<span>${escapeHtml(t("thermal.policyTab"))}</span></button>
      <button type="button" role="tab" data-thermal-panel="curve" aria-selected="${state.thermalPanel === "curve"}">${icon("pulse")}<span>${escapeHtml(t("fanCurve.tab"))}</span></button>
    </div>
    ${state.thermalPanel === "curve" ? renderFanCurvePanel(data) : renderThermalPolicyPanel(data)}`;
  $$('[data-thermal-panel]', host).forEach((button) => button.addEventListener("click", () => {
    state.thermalPanel = button.dataset.thermalPanel;
    renderThermalTool();
  }));
  if (state.thermalPanel === "policy") bindThermalPolicyPanel(host);
  else bindFanCurvePanel(host);
}

function renderThermalPolicyPanel(data) {
  const curveConfigured = Boolean(data.fanCurve?.config);
  return `
    <div class="thermal-summary">
      <span><i>${escapeHtml(t("thermal.current"))}</i><b>${escapeHtml(data.currentPolicy || "—")}</b></span>
      <span><i>${escapeHtml(t("thermal.saved"))}</i><b>${escapeHtml(data.persistedPolicy || "—")}</b></span>
      <span><i>${escapeHtml(t("thermal.recommended"))}</i><b>${escapeHtml(data.recommendedPolicy || "—")}</b></span>
    </div>
    ${curveConfigured ? `<div class="tool-warning">${escapeHtml(t("fanCurve.policyLocked"))}</div>` : data.pwmFanDetected ? `<div class="tool-warning">${escapeHtml(t("thermal.pwmWarning"))}</div>` : ""}
    <div class="thermal-policies">${data.availablePolicies.map((policy) => {
      const key = ["step_wise", "power_allocator"].includes(policy) ? policy : "other";
      const blocked = curveConfigured || (data.pwmFanDetected && policy === "power_allocator");
      return `<label class="thermal-policy ${blocked ? "is-blocked" : ""}"><input type="radio" name="thermal-policy" value="${escapeHtml(policy)}" ${policy === state.thermalPolicy ? "checked" : ""} ${blocked ? "disabled" : ""} /><span><strong>${escapeHtml(policy)}</strong><i>${escapeHtml(t(`thermal.policyHint.${key}`))}</i></span>${policy === data.recommendedPolicy ? `<b>${escapeHtml(t("thermal.recommended"))}</b>` : ""}</label>`;
    }).join("")}</div>
    <h3 class="tool-section-title">${escapeHtml(t("thermal.zone"))}</h3>
    <div class="thermal-zone-list">${data.zones.map((zone) => `<div><span><strong>${escapeHtml(zone.kind)}</strong><i>${escapeHtml(zone.id)}</i></span><b>${zone.temperatureC == null ? "—" : `${formatNumber(zone.temperatureC, 1)} °C`}</b></div>`).join("")}</div>
    <h3 class="tool-section-title">${escapeHtml(t("thermal.cooling"))}</h3>
    <div class="cooling-list">${data.coolingDevices.length ? data.coolingDevices.map((device) => `<div><span><strong>${escapeHtml(device.kind)}</strong><i>${escapeHtml(device.id)}</i></span><b>${escapeHtml(t("thermal.state", { current: device.currentState ?? "—", max: device.maxState ?? "—" }))}</b></div>`).join("") : `<p>${escapeHtml(t("thermal.noCooling"))}</p>`}</div>
    <label class="confirm-line hardware-confirm"><input type="checkbox" data-thermal-confirm ${curveConfigured ? "disabled" : ""} /><span>${escapeHtml(t("thermal.confirm"))}</span></label>
    <button class="execute-button hardware-execute" type="button" data-thermal-apply disabled><span>${escapeHtml(t("thermal.apply"))}</span>${icon("run")}</button>
    <div class="drawer-result" data-thermal-result hidden></div>`;
}

function bindThermalPolicyPanel(host) {
  $$('input[name="thermal-policy"]', host).forEach((input) => input.addEventListener("change", (event) => {
    state.thermalPolicy = event.currentTarget.value;
  }));
  $("[data-thermal-confirm]", host)?.addEventListener("change", (event) => {
    $("[data-thermal-apply]", host).disabled = !event.currentTarget.checked || !state.thermalPolicy;
  });
  $("[data-thermal-apply]", host)?.addEventListener("click", applyThermalPolicy);
}

function fanCurvePointRow(point, index, count, mutable) {
  return `<div class="fan-curve-point" data-fan-point="${index}">
    <b>${index + 1}</b>
    <label><span>${escapeHtml(t("fanCurve.temperature"))}</span><input type="number" min="0" max="120" step="1" value="${escapeHtml(point.temperatureC)}" data-fan-temperature ${mutable ? "" : "disabled"} /><i>°C</i></label>
    <label><span>${escapeHtml(t("fanCurve.speed"))}</span><input type="range" min="0" max="100" step="1" value="${escapeHtml(point.speedPercent)}" data-fan-speed ${mutable ? "" : "disabled"} /><output>${escapeHtml(point.speedPercent)}%</output></label>
    <button type="button" data-fan-remove aria-label="${escapeHtml(t("fanCurve.removePoint"))}" ${mutable && count > 2 ? "" : "disabled"}>${icon("close")}</button>
  </div>`;
}

function renderFanCurvePanel(data) {
  const status = data.fanCurve;
  if (!status?.supported) {
    return `<div class="hardware-tool-empty">${escapeHtml(hardwareReason(status?.unavailableReason) || t("fanCurve.unavailable"))}</div>`;
  }
  const draft = state.fanCurveDraft || defaultFanCurveConfig(status);
  const selectedZone = status.zones.find((zone) => zone.id === draft.zoneId) || status.zones[0];
  const selectedDevice = status.coolingDevices.find((device) => device.id === draft.coolingDeviceId) || status.coolingDevices[0];
  const configured = Boolean(status.config);
  const statusKey = status.active ? "active" : configured ? "stopped" : "inactive";
  const activeZone = configured ? status.zones.find((zone) => zone.id === status.config.zoneId) : null;
  const activeDevice = configured ? status.coolingDevices.find((device) => device.id === status.config.coolingDeviceId) : null;
  const statusDetail = configured
    ? t("fanCurve.statusDetail", { zone: activeZone?.kind || activeZone?.id || status.config.zoneId, state: activeDevice?.currentState ?? "—", max: activeDevice?.maxState ?? "—" })
    : t("fanCurve.statusKernelDetail");
  return `
    <div class="fan-curve-status" data-state="${statusKey}">
      <span><i></i><b>${escapeHtml(t(`fanCurve.status.${statusKey}`))}</b><small>${escapeHtml(statusDetail)}</small></span>
      ${configured ? `<button type="button" class="text-button" data-fan-disable-preview>${escapeHtml(t("fanCurve.disable"))}</button>` : ""}
    </div>
    <div class="fan-curve-chart" data-fan-chart>${fanCurveChart(draft.points, selectedZone?.temperatureC)}</div>
    <div class="fan-curve-targets">
      <label class="tool-field"><span>${escapeHtml(t("fanCurve.sensor"))}</span><select data-fan-zone ${status.mutable ? "" : "disabled"}>${status.zones.filter((zone) => zone.supportsUserSpace).map((zone) => `<option value="${escapeHtml(zone.id)}" ${zone.id === draft.zoneId ? "selected" : ""}>${escapeHtml(zone.kind)} · ${zone.temperatureC == null ? "—" : `${formatNumber(zone.temperatureC, 1)} °C`}</option>`).join("")}</select></label>
      <label class="tool-field"><span>${escapeHtml(t("fanCurve.device"))}</span><select data-fan-device ${status.mutable ? "" : "disabled"}>${status.coolingDevices.map((device) => `<option value="${escapeHtml(device.id)}" ${device.id === draft.coolingDeviceId ? "selected" : ""}>${escapeHtml(device.kind)} · ${escapeHtml(t("thermal.state", { current: device.currentState ?? "—", max: device.maxState }))}</option>`).join("")}</select></label>
    </div>
    <div class="fan-curve-heading"><div><h3>${escapeHtml(t("fanCurve.points"))}</h3><p>${escapeHtml(t("fanCurve.pointsHint"))}</p></div><button type="button" class="text-button" data-fan-add ${status.mutable && draft.points.length < 8 ? "" : "disabled"}>${escapeHtml(t("fanCurve.addPoint"))}</button></div>
    <div class="fan-curve-points">${draft.points.map((point, index) => fanCurvePointRow(point, index, draft.points.length, status.mutable)).join("")}</div>
    <div class="fan-curve-settings">
      <label><span>${escapeHtml(t("fanCurve.hysteresis"))}</span><input type="number" min="0" max="10" step="0.5" value="${escapeHtml(draft.hysteresisC)}" data-fan-hysteresis ${status.mutable ? "" : "disabled"} /><i>°C</i></label>
      <label><span>${escapeHtml(t("fanCurve.poll"))}</span><select data-fan-poll ${status.mutable ? "" : "disabled"}>${[500, 1000, 2000, 5000, 10000].map((value) => `<option value="${value}" ${Number(draft.pollIntervalMs) === value ? "selected" : ""}>${value < 1000 ? `${value} ms` : `${value / 1000} s`}</option>`).join("")}</select></label>
    </div>
    ${!status.mutable && status.unavailableReason ? `<div class="tool-warning">${escapeHtml(hardwareReason(status.unavailableReason))}</div>` : ""}
    <button class="secondary-button fan-curve-preview" type="button" data-fan-preview ${status.mutable ? "" : "disabled"}><span>${escapeHtml(t("fanCurve.preview"))}</span>${icon("run")}</button>
    <div data-fan-plan></div>`;
}

function invalidateFanCurvePreview() {
  resetFanCurvePlan();
  const host = $("[data-fan-plan]");
  if (host) host.innerHTML = "";
}

function updateFanCurveChart() {
  const chart = $("[data-fan-chart]");
  if (!chart) return;
  const zone = state.hardwareData?.fanCurve?.zones?.find((item) => item.id === state.fanCurveDraft?.zoneId);
  chart.innerHTML = fanCurveChart(state.fanCurveDraft?.points, zone?.temperatureC);
}

function bindFanCurvePanel(host) {
  $("[data-fan-zone]", host)?.addEventListener("change", (event) => {
    state.fanCurveDraft.zoneId = event.currentTarget.value;
    invalidateFanCurvePreview();
    renderThermalTool();
  });
  $("[data-fan-device]", host)?.addEventListener("change", (event) => {
    state.fanCurveDraft.coolingDeviceId = event.currentTarget.value;
    invalidateFanCurvePreview();
    renderThermalTool();
  });
  $$("[data-fan-point]", host).forEach((row) => {
    const index = Number(row.dataset.fanPoint);
    $("[data-fan-temperature]", row).addEventListener("input", (event) => {
      state.fanCurveDraft.points[index].temperatureC = Number(event.currentTarget.value);
      invalidateFanCurvePreview();
      updateFanCurveChart();
    });
    $("[data-fan-speed]", row).addEventListener("input", (event) => {
      state.fanCurveDraft.points[index].speedPercent = Number(event.currentTarget.value);
      $("output", row).textContent = `${event.currentTarget.value}%`;
      invalidateFanCurvePreview();
      updateFanCurveChart();
    });
    $("[data-fan-remove]", row).addEventListener("click", () => {
      state.fanCurveDraft.points.splice(index, 1);
      invalidateFanCurvePreview();
      renderThermalTool();
    });
  });
  $("[data-fan-add]", host)?.addEventListener("click", () => {
    const last = state.fanCurveDraft.points.at(-1) || { temperatureC: 40, speedPercent: 20 };
    state.fanCurveDraft.points.push({ temperatureC: Math.min(90, Number(last.temperatureC) + 8), speedPercent: Math.min(100, Number(last.speedPercent) + 10) });
    invalidateFanCurvePreview();
    renderThermalTool();
  });
  $("[data-fan-hysteresis]", host)?.addEventListener("input", (event) => {
    state.fanCurveDraft.hysteresisC = Number(event.currentTarget.value);
    invalidateFanCurvePreview();
  });
  $("[data-fan-poll]", host)?.addEventListener("change", (event) => {
    state.fanCurveDraft.pollIntervalMs = Number(event.currentTarget.value);
    invalidateFanCurvePreview();
  });
  $("[data-fan-preview]", host)?.addEventListener("click", () => previewFanCurve(fanCurveRequest()));
  $("[data-fan-disable-preview]", host)?.addEventListener("click", () => previewFanCurve({ enabled: false, config: null }));
  if (state.fanCurvePlan) renderFanCurvePlan();
}

async function previewFanCurve(requestBody) {
  const validationKey = fanCurveValidationKey(requestBody);
  if (validationKey) {
    const host = $("[data-fan-plan]");
    if (host) host.innerHTML = `<div class="tool-warning fan-curve-error">${escapeHtml(t(validationKey))}</div>`;
    return;
  }
  const previewVersion = state.fanCurvePreviewVersion + 1;
  state.fanCurvePreviewVersion = previewVersion;
  state.fanCurvePlan = null;
  const button = requestBody.enabled ? $("[data-fan-preview]") : $("[data-fan-disable-preview]");
  if (button) {
    button.disabled = true;
    const copy = $("span", button);
    if (copy) copy.textContent = t("fanCurve.previewing");
  }
  try {
    const plan = await transport.planFanCurve(requestBody);
    if (previewVersion !== state.fanCurvePreviewVersion || state.selectedHardware !== "thermal") return;
    const current = requestBody.enabled ? fanCurveRequest() : { enabled: false, config: null };
    if (!sameFanCurveRequest(current, requestBody) || !sameFanCurveRequest(plan.request, requestBody)) {
      resetFanCurvePlan();
      renderThermalTool();
      toast(t("toast.failed"), t("api.stale_plan"), true);
      return;
    }
    state.fanCurvePlan = plan;
    renderThermalTool();
  } catch (error) {
    if (previewVersion !== state.fanCurvePreviewVersion || state.selectedHardware !== "thermal") return;
    const host = $("[data-fan-plan]");
    if (host) host.innerHTML = `<div class="tool-warning fan-curve-error">${escapeHtml(displayError(error))}</div>`;
    if (button) {
      button.disabled = false;
      const copy = $("span", button);
      if (copy) copy.textContent = t(requestBody.enabled ? "fanCurve.preview" : "fanCurve.disable");
    }
  }
}

function renderFanCurvePlan() {
  const plan = state.fanCurvePlan;
  const host = $("[data-fan-plan]");
  if (!host || !plan) return;
  const enabled = plan.request.enabled;
  host.innerHTML = `<section class="hardware-plan fan-curve-plan">
    <div class="hardware-plan-head"><strong>${escapeHtml(t(enabled ? "fanCurve.planEnable" : "fanCurve.planDisable"))}</strong><span>${escapeHtml(plan.synthetic ? t("activity.simulated") : t("drawer.root"))}</span></div>
    ${enabled ? `<div class="fan-curve-resolved">${plan.resolvedPoints.map((point) => `<span><b>${formatNumber(point.temperatureC, 0)} °C</b><i>${point.speedPercent}% · ${escapeHtml(t("fanCurve.state", { state: point.coolingState }))}</i></span>`).join("")}</div>` : `<p class="fan-curve-restore">${escapeHtml(t("fanCurve.restore", { policy: plan.previousPolicy || "—" }))}</p>`}
    ${(plan.warnings || []).map((warning) => `<div class="tool-warning">${escapeHtml(t(`fanCurve.warning.${warning}`))}</div>`).join("")}
    <label class="confirm-line hardware-confirm"><input type="checkbox" data-fan-confirm /><span>${escapeHtml(t(enabled ? "fanCurve.confirmEnable" : "fanCurve.confirmDisable"))}</span></label>
    <button class="execute-button hardware-execute" type="button" data-fan-apply disabled><span>${escapeHtml(t(enabled ? "fanCurve.apply" : "fanCurve.disable"))}</span>${icon("run")}</button>
    <div class="drawer-result" data-fan-result hidden></div>
  </section>`;
  $("[data-fan-confirm]", host).addEventListener("change", (event) => {
    $("[data-fan-apply]", host).disabled = !event.currentTarget.checked;
  });
  $("[data-fan-apply]", host).addEventListener("click", applyFanCurve);
}

async function applyFanCurve() {
  const plan = state.fanCurvePlan;
  if (!plan) return;
  const current = plan.request.enabled ? fanCurveRequest() : { enabled: false, config: null };
  if (!sameFanCurveRequest(current, plan.request)) {
    resetFanCurvePlan();
    renderThermalTool();
    toast(t("toast.failed"), t("api.stale_plan"), true);
    return;
  }
  const button = $("[data-fan-apply]");
  const result = $("[data-fan-result]");
  button.disabled = true;
  $("span", button).textContent = t("fanCurve.applying");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("drawer.runState");
  try {
    const response = await transport.applyFanCurve(plan.request, plan.planToken, true);
    result.textContent = response.run.synthetic ? t("sources.planned") : t(plan.request.enabled ? "fanCurve.applied" : "fanCurve.disabled");
    toast(response.run.synthetic ? t("toast.dryRun") : t("fanCurve.updated"), t(plan.request.enabled ? "fanCurve.applied" : "fanCurve.disabled"));
    if (response.run.synthetic) {
      $("span", button).textContent = t(plan.request.enabled ? "fanCurve.apply" : "fanCurve.disable");
      button.disabled = false;
    }
    if (!response.run.synthetic) {
      const [thermal, fanCurve] = await Promise.all([transport.thermalStatus(), transport.fanCurveStatus()]);
      state.hardwareData = { ...thermal, fanCurve };
      state.thermalPolicy = thermal.currentPolicy;
      initializeFanCurveState(fanCurve);
      renderThermalTool();
    }
    await refreshAll({ quiet: true });
  } catch (error) {
    result.classList.add("is-error");
    result.textContent = displayError(error);
    $("span", button).textContent = t(plan.request.enabled ? "fanCurve.apply" : "fanCurve.disable");
    button.disabled = false;
  }
}

async function applyThermalPolicy() {
  if (!state.thermalPolicy) return;
  const button = $("[data-thermal-apply]");
  const result = $("[data-thermal-result]");
  button.disabled = true;
  $("span", button).textContent = t("thermal.applying");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("drawer.runState");
  try {
    const run = await transport.applyThermalPolicy(state.thermalPolicy, true);
    result.textContent = run.synthetic ? t("sources.planned") : t("thermal.applied");
    toast(run.synthetic ? t("toast.dryRun") : t("thermal.applied"), state.thermalPolicy);
    if (!run.synthetic) {
      const [thermal, fanCurve] = await Promise.all([transport.thermalStatus(), transport.fanCurveStatus()]);
      state.hardwareData = { ...thermal, fanCurve };
      state.thermalPolicy = thermal.currentPolicy;
      initializeFanCurveState(fanCurve);
      renderThermalTool();
    }
    await refreshAll({ quiet: true });
  } catch (error) {
    const detail = displayError(error);
    result.classList.add("is-error");
    result.textContent = detail;
    $("span", button).textContent = t("thermal.apply");
    button.disabled = false;
  }
}

function standaloneLeds(data = state.hardwareData) {
  return (data?.leds || []).filter((led) => !led.rgbGroup);
}

function defaultRgbLedConfig(group) {
  return {
    groupId: group?.id || "",
    mode: "solid",
    red: 91,
    green: 104,
    blue: 181,
    brightness: 80,
    cycleMs: 5_000,
  };
}

function preferredLedTrigger(data, led) {
  const saved = data.savedState?.triggers?.[led.id];
  if (led.availableTriggers.includes(saved)) return saved;
  if (led.availableTriggers.includes(led.currentTrigger)) return led.currentTrigger;
  return led.availableTriggers[0] || "";
}

function initializeLedState(data) {
  const leds = standaloneLeds(data);
  const led = leds[0];
  state.ledSelection = led ? {
    ledId: led.id,
    trigger: preferredLedTrigger(data, led),
  } : null;
  const group = data.rgbGroups?.[0];
  state.rgbLedConfig = group
    ? { ...defaultRgbLedConfig(group), ...(data.savedState?.rgb?.[group.id] || {}), groupId: group.id }
    : null;
  state.ledPanel = leds.length ? "status" : "rgb";
}

function rgbLedHex(config) {
  const channel = (value) => Math.max(0, Math.min(255, Number(value) || 0)).toString(16).padStart(2, "0");
  return `#${channel(config?.red)}${channel(config?.green)}${channel(config?.blue)}`;
}

function renderLedTool() {
  const data = state.hardwareData;
  const host = $("[data-hardware-body]");
  if (!data.supported) {
    host.innerHTML = `<div class="hardware-tool-empty">${escapeHtml(data.unavailableReason || t("hardware.unavailable"))}</div>`;
    return;
  }
  const leds = standaloneLeds(data);
  const groups = data.rgbGroups || [];
  if (state.ledPanel === "status" && !leds.length && groups.length) state.ledPanel = "rgb";
  if (state.ledPanel === "rgb" && !groups.length && leds.length) state.ledPanel = "status";
  host.innerHTML = `
    <div class="tool-fact-line"><span>${escapeHtml(t("led.detected", { status: leds.length, groups: groups.length }))}</span><b>LED CLASS</b></div>
    <div class="led-tabs" role="tablist" aria-label="${escapeHtml(t("led.title"))}">
      <button type="button" role="tab" data-led-panel="status" aria-selected="${state.ledPanel === "status"}" ${leds.length ? "" : "disabled"}>${icon("led")}<span>${escapeHtml(t("led.statusTab"))}</span><b>${leds.length}</b></button>
      <button type="button" role="tab" data-led-panel="rgb" aria-selected="${state.ledPanel === "rgb"}" ${groups.length ? "" : "disabled"}>${icon("sun")}<span>${escapeHtml(t("led.rgbTab"))}</span><b>${groups.length}</b></button>
    </div>
    <div data-led-controls>${state.ledPanel === "rgb" ? renderRgbLedControls(data) : renderStatusLedControls(data)}</div>`;
  $$('[data-led-panel]', host).forEach((button) => button.addEventListener("click", () => {
    state.ledPanel = button.dataset.ledPanel;
    renderLedTool();
  }));
  bindLedControls();
}

function renderStatusLedControls(data) {
  const leds = standaloneLeds(data);
  if (!leds.length) return `<div class="hardware-tool-empty compact">${escapeHtml(t("led.noStatus"))}</div>`;
  const selected = leds.find((led) => led.id === state.ledSelection?.ledId) || leds[0];
  if (!state.ledSelection || selected.id !== state.ledSelection.ledId) {
    state.ledSelection = {
      ledId: selected.id,
      trigger: preferredLedTrigger(data, selected),
    };
  }
  const saved = data.savedState?.triggers?.[selected.id] || "—";
  const brightness = selected.maxBrightness
    ? Math.round((selected.brightness || 0) / selected.maxBrightness * 100)
    : null;
  return `<section class="led-control-panel" role="tabpanel">
    <div class="led-status-orbit" aria-hidden="true"><span style="--led-level:${brightness ?? 35}%"></span></div>
    <div class="led-control-fields">
      <label class="tool-field"><span>${escapeHtml(t("led.device"))}</span><select data-led-device ${data.mutable ? "" : "disabled"}>${leds.map((led) => `<option value="${escapeHtml(led.id)}" ${led.id === selected.id ? "selected" : ""}>${escapeHtml(led.id)}</option>`).join("")}</select></label>
      <div class="led-facts"><span><i>${escapeHtml(t("led.current"))}</i><b>${escapeHtml(selected.currentTrigger || "—")}</b></span><span><i>${escapeHtml(t("led.saved"))}</i><b>${escapeHtml(saved)}</b></span><span><i>${escapeHtml(t("led.brightness"))}</i><b>${brightness == null ? "—" : `${brightness}%`}</b></span></div>
      <label class="tool-field"><span>${escapeHtml(t("led.trigger"))}</span><select data-led-trigger ${data.mutable ? "" : "disabled"}>${selected.availableTriggers.map((trigger) => `<option value="${escapeHtml(trigger)}" ${trigger === state.ledSelection.trigger ? "selected" : ""}>${escapeHtml(trigger)}</option>`).join("")}</select></label>
    </div>
    ${!data.mutable && data.unavailableReason ? `<div class="tool-warning">${escapeHtml(data.unavailableReason)}</div>` : ""}
    <label class="confirm-line hardware-confirm"><input type="checkbox" data-led-confirm ${data.mutable ? "" : "disabled"} /><span>${escapeHtml(t("led.confirm"))}</span></label>
    <button class="execute-button hardware-execute" type="button" data-led-apply="trigger" disabled><span>${escapeHtml(t("led.applyTrigger"))}</span>${icon("run")}</button>
    <div class="drawer-result" data-led-result hidden></div>
  </section>`;
}

function renderRgbLedControls(data) {
  const groups = data.rgbGroups || [];
  if (!groups.length) return `<div class="hardware-tool-empty compact">${escapeHtml(t("led.noRgb"))}</div>`;
  const group = groups.find((item) => item.id === state.rgbLedConfig?.groupId) || groups[0];
  if (!state.rgbLedConfig || state.rgbLedConfig.groupId !== group.id) {
    state.rgbLedConfig = { ...defaultRgbLedConfig(group), ...(data.savedState?.rgb?.[group.id] || {}), groupId: group.id };
  }
  const config = state.rgbLedConfig;
  const color = rgbLedHex(config);
  return `<section class="led-control-panel" role="tabpanel">
    <div class="rgb-led-stage" data-mode="${escapeHtml(config.mode)}" style="--led-color:${color};--led-level:${config.brightness / 100};--led-cycle:${config.cycleMs}ms;--led-breath-phase:${config.cycleMs / 2}ms">
      <span class="rgb-led-glow"></span><span class="rgb-led-core"></span><i>${escapeHtml(t("led.preview"))}</i>
    </div>
    <label class="tool-field"><span>${escapeHtml(t("led.rgbGroup"))}</span><select data-rgb-group ${data.mutable ? "" : "disabled"}>${groups.map((item) => `<option value="${escapeHtml(item.id)}" ${item.id === group.id ? "selected" : ""}>${escapeHtml(item.id)} · RGB</option>`).join("")}</select></label>
    <fieldset class="led-mode-field" ${data.mutable ? "" : "disabled"}><legend>${escapeHtml(t("led.mode"))}</legend><div>${["solid", "breath", "rainbow"].map((mode) => `<label><input type="radio" name="led-mode" value="${mode}" ${config.mode === mode ? "checked" : ""} /><span>${escapeHtml(t(`led.mode.${mode}`))}</span></label>`).join("")}</div></fieldset>
    <div class="led-adjust-grid">
      <label class="led-color-field"><span>${escapeHtml(t("led.color"))}</span><input type="color" value="${color}" data-led-color ${config.mode === "rainbow" || !data.mutable ? "disabled" : ""} /><b>${color.toUpperCase()}</b></label>
      <label class="led-range-field"><span>${escapeHtml(t("led.brightness"))}</span><input type="range" min="0" max="100" step="1" value="${config.brightness}" data-led-brightness ${data.mutable ? "" : "disabled"} /><b data-led-brightness-value>${config.brightness}%</b></label>
      <label class="led-cycle-field"><span>${escapeHtml(t("led.cycle"))}</span><input type="number" min="200" max="60000" step="100" value="${config.cycleMs}" data-led-cycle ${data.mutable ? "" : "disabled"} /><b>${escapeHtml(t("led.milliseconds"))}</b></label>
    </div>
    ${!data.mutable && data.unavailableReason ? `<div class="tool-warning">${escapeHtml(data.unavailableReason)}</div>` : ""}
    <label class="confirm-line hardware-confirm"><input type="checkbox" data-led-confirm ${data.mutable ? "" : "disabled"} /><span>${escapeHtml(t("led.confirm"))}</span></label>
    <button class="execute-button hardware-execute" type="button" data-led-apply="rgb" disabled><span>${escapeHtml(t("led.applyRgb"))}</span>${icon("run")}</button>
    <div class="drawer-result" data-led-result hidden></div>
  </section>`;
}

function bindLedControls() {
  const host = $("[data-hardware-body]");
  $("[data-led-device]", host)?.addEventListener("change", (event) => {
    const led = standaloneLeds().find((item) => item.id === event.currentTarget.value);
    state.ledSelection = led ? {
      ledId: led.id,
      trigger: preferredLedTrigger(state.hardwareData, led),
    } : null;
    renderLedTool();
  });
  $("[data-led-trigger]", host)?.addEventListener("change", (event) => {
    state.ledSelection.trigger = event.currentTarget.value;
    resetLedConfirmation(host);
  });
  $("[data-rgb-group]", host)?.addEventListener("change", (event) => {
    const group = state.hardwareData.rgbGroups.find((item) => item.id === event.currentTarget.value);
    state.rgbLedConfig = { ...defaultRgbLedConfig(group), ...(state.hardwareData.savedState?.rgb?.[group.id] || {}), groupId: group.id };
    renderLedTool();
  });
  $$('input[name="led-mode"]', host).forEach((input) => input.addEventListener("change", (event) => {
    state.rgbLedConfig.mode = event.currentTarget.value;
    renderLedTool();
  }));
  $("[data-led-color]", host)?.addEventListener("input", (event) => {
    const value = event.currentTarget.value;
    state.rgbLedConfig.red = parseInt(value.slice(1, 3), 16);
    state.rgbLedConfig.green = parseInt(value.slice(3, 5), 16);
    state.rgbLedConfig.blue = parseInt(value.slice(5, 7), 16);
    const stage = $(".rgb-led-stage", host);
    stage.style.setProperty("--led-color", value);
    $(".led-color-field b", host).textContent = value.toUpperCase();
    resetLedConfirmation(host);
  });
  $("[data-led-brightness]", host)?.addEventListener("input", (event) => {
    state.rgbLedConfig.brightness = Number(event.currentTarget.value);
    $("[data-led-brightness-value]", host).textContent = `${state.rgbLedConfig.brightness}%`;
    $(".rgb-led-stage", host).style.setProperty("--led-level", state.rgbLedConfig.brightness / 100);
    resetLedConfirmation(host);
  });
  $("[data-led-cycle]", host)?.addEventListener("input", (event) => {
    const cycleMs = Number(event.currentTarget.value);
    if (cycleMs >= 200 && cycleMs <= 60_000) {
      state.rgbLedConfig.cycleMs = cycleMs;
      $(".rgb-led-stage", host).style.setProperty("--led-cycle", `${cycleMs}ms`);
      $(".rgb-led-stage", host).style.setProperty("--led-breath-phase", `${cycleMs / 2}ms`);
    }
    resetLedConfirmation(host);
  });
  $("[data-led-cycle]", host)?.addEventListener("change", (event) => {
    state.rgbLedConfig.cycleMs = Math.max(200, Math.min(60_000, Number(event.currentTarget.value) || 5_000));
    event.currentTarget.value = state.rgbLedConfig.cycleMs;
    $(".rgb-led-stage", host).style.setProperty("--led-cycle", `${state.rgbLedConfig.cycleMs}ms`);
    $(".rgb-led-stage", host).style.setProperty("--led-breath-phase", `${state.rgbLedConfig.cycleMs / 2}ms`);
    resetLedConfirmation(host);
  });
  $("[data-led-confirm]", host)?.addEventListener("change", (event) => {
    $("[data-led-apply]", host).disabled = !event.currentTarget.checked;
  });
  $("[data-led-apply]", host)?.addEventListener("click", applyLedConfiguration);
}

function resetLedConfirmation(host) {
  const confirmation = $("[data-led-confirm]", host);
  const apply = $("[data-led-apply]", host);
  if (confirmation) confirmation.checked = false;
  if (apply) apply.disabled = true;
}

async function applyLedConfiguration(event) {
  const kind = event.currentTarget.dataset.ledApply;
  const labelKey = kind === "rgb" ? "led.applyRgb" : "led.applyTrigger";
  const button = event.currentTarget;
  const result = $("[data-led-result]");
  button.disabled = true;
  $("span", button).textContent = t("led.applying");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("drawer.runState");
  try {
    const run = kind === "rgb"
      ? await transport.applyRgbLed(state.rgbLedConfig, true)
      : await transport.applyLedTrigger(state.ledSelection.ledId, state.ledSelection.trigger, true);
    result.textContent = run.synthetic ? t("sources.planned") : t("led.applied");
    toast(run.synthetic ? t("toast.dryRun") : t("led.applied"), kind === "rgb" ? state.rgbLedConfig.mode : state.ledSelection.trigger);
    $("span", button).textContent = t(labelKey);
    const confirmation = $("[data-led-confirm]");
    if (confirmation) confirmation.checked = false;
    if (!run.synthetic) {
      const panel = state.ledPanel;
      state.hardwareData = await transport.ledStatus();
      initializeLedState(state.hardwareData);
      state.ledPanel = panel;
      renderLedTool();
    }
    await refreshAll({ quiet: true });
  } catch (error) {
    const detail = displayError(error);
    result.classList.add("is-error");
    result.textContent = detail;
    $("span", button).textContent = t(labelKey);
    button.disabled = false;
  }
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

function helpResourceUrl(resource) {
  return resource.url || localizedDocsUrl(resource.path);
}

function renderHelpResource(resource) {
  return `<a class="help-resource-link" href="${escapeHtml(helpResourceUrl(resource))}" target="_blank" rel="noopener noreferrer">
    ${icon("docs")}<span><strong>${escapeHtml(t(`help.resource.${resource.kind}.title`))}</strong><span>${escapeHtml(t(`help.resource.${resource.kind}.description`))}</span></span>${icon("external")}
  </a>`;
}

function renderContactLink(channel) {
  return `<a class="contact-link" data-channel="${escapeHtml(channel.id)}" href="${escapeHtml(channel.url)}" target="_blank" rel="noopener noreferrer">
    <span class="contact-mark" aria-hidden="true">${escapeHtml(channel.mark)}</span><span class="contact-link-copy"><strong>${escapeHtml(t(`contact.channel.${channel.id}.name`))}</strong><span>${escapeHtml(t(`contact.channel.${channel.id}.detail`))}</span></span>${icon("external")}
  </a>`;
}

function renderContactLinks() {
  const host = $("[data-contact-links]");
  if (host) host.innerHTML = contactChannels.map(renderContactLink).join("");
}

function renderHelp() {
  const identity = state.snapshot?.identity;
  const profile = helpProfile(identity);
  const board = profile?.name || identity?.product || t("board.detecting");
  const soc = identity?.soc || t("provider.unknown");
  const resources = profile?.resources || genericHelpResources;

  setText("[data-help-board]", t("help.boardBadge", { board, soc }));
  setText("[data-help-library-title]", t("help.libraryTitle", { board }));
  setText("[data-help-library-description]", t(profile ? "help.libraryDescription" : "help.libraryGeneric", { board }));
  $("[data-help-links]").innerHTML = resources.map(renderHelpResource).join("");
  $("[data-help-faq]").innerHTML = helpFaqs.map((faq) => `<details class="faq-item">
    <summary><span>${escapeHtml(t(`help.faq.${faq}.question`, { board }))}</span>${icon("plus")}</summary>
    <div class="faq-answer">${escapeHtml(t(`help.faq.${faq}.answer`, { board }))}</div>
  </details>`).join("");
  renderContactLinks();
}

function openContact(event) {
  state.contactInvoker = event?.currentTarget || document.activeElement;
  renderContactLinks();
  const dialog = $("[data-contact-dialog]");
  if (!dialog.open) openDialog(dialog);
  requestAnimationFrame(() => $("[data-contact-close]").focus());
}

function closeContact() {
  const dialog = $("[data-contact-dialog]");
  if (dialog.open) dismissDialog(dialog);
  const invoker = state.contactInvoker;
  state.contactInvoker = null;
  requestAnimationFrame(() => invoker?.focus?.());
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
  $$('[data-contact-open]').forEach((button) => button.addEventListener("click", openContact));
  $("[data-contact-close]").addEventListener("click", closeContact);
  $("[data-contact-dialog]").addEventListener("cancel", (event) => { event.preventDefault(); closeContact(); });
  $("[data-task-close]").addEventListener("click", closeAction);
  $("[data-task-drawer]").addEventListener("cancel", (event) => { event.preventDefault(); closeAction(); });
  $("[data-task-confirm]").addEventListener("change", (event) => {
    $("[data-task-execute]").disabled = !state.selectedAction?.available || !event.currentTarget.checked;
  });
  $("[data-task-execute]").addEventListener("click", executeSelectedAction);
  $("[data-hardware-close]").addEventListener("click", closeHardwareTool);
  $("[data-hardware-drawer]").addEventListener("cancel", (event) => { event.preventDefault(); closeHardwareTool(); });
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
  window.addEventListener("rsetup:locale", () => { applyStaticTranslations(); renderAll(); renderActionDetails(); renderHardwareTool(); });
  window.addEventListener("keydown", (event) => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); openCommands(); }
    if (event.key === "Escape" && $("[data-command-dialog]").open) {
      event.preventDefault();
      dismissDialog($("[data-command-dialog]"));
    } else if (event.key === "Escape" && $("[data-task-drawer]").classList.contains("is-open")) {
      closeAction();
    } else if (event.key === "Escape" && $("[data-hardware-drawer]").classList.contains("is-open")) {
      closeHardwareTool();
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

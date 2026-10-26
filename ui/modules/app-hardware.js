function hardwareReason(reason) {
  return i18n.hardwareReason(reason);
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
    storage: "storageTool",
  }[id] || "hardware";
  return { title: t(`${prefix}.title`), description: t(`${prefix}.description`) };
}

function overlayDisplayCopy(overlay) {
  if (!overlay) return {};
  overlay = { ...overlay, description: overlay.description?.replace(/\\n/g, " ") };
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
  state.storageRefreshPromise = null;
  state.storageRefreshing = false;
  state.storageRefreshError = null;
  state.storageStale = false;
  state.storageRefreshedAt = null;
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
  if (id === "storage") {
    void refreshStorageTool();
    return;
  }
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
  state.storageRefreshPromise = null;
  state.storageRefreshing = false;
  state.storageRefreshError = null;
  state.storageStale = false;
  state.storageRefreshedAt = null;
}

function renderHardwareError(message) {
  $("[data-hardware-body]").innerHTML = `<div class="hardware-tool-error">${icon("pulse")}<strong>${escapeHtml(t("toast.failed"))}</strong><span>${escapeHtml(message)}</span></div>`;
}

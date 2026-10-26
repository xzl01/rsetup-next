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
  async benchmarkSource(providerId) {
    if (tauriInvoke) return tauriInvoke("benchmark_source", { providerId });
    return request("/api/v1/sources/benchmark", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ providerId }),
    });
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
  async authorizeOverlays() {
    if (tauriInvoke) return tauriInvoke("authorize_overlay_read");
    return request("/api/v1/hardware/overlays/authorize", { method: "POST" });
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
  async storageStatus() {
    if (tauriInvoke) return tauriInvoke("storage_status");
    return request("/api/v1/hardware/storage");
  },
  // System image install. The image is always an identifier from the fixed
  // server-side catalogue: the client never submits a path, and the
  // acknowledgement is always the typed device name, never a boolean.
  async installTargets() {
    if (tauriInvoke) return tauriInvoke("system_install_targets");
    return request("/api/v1/storage/install/targets");
  },
  async installPlan(targetId, imageId) {
    const payload = installPlanRequest(targetId, imageId);
    if (tauriInvoke) return tauriInvoke("system_install_plan", payload);
    return request("/api/v1/storage/install/plan", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify(payload),
    });
  },
  async installApply(payload) {
    if (tauriInvoke) return tauriInvoke("system_install_apply", payload);
    return request("/api/v1/storage/install/apply", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify(payload),
    });
  },
  // Desktop shell only: the native chooser runs in Rust and answers with an
  // opaque handle plus display metadata. The browser never has this capability
  // and the page never renders an entry for it there.
  async pickInstallImage() {
    if (!tauriInvoke) return null;
    return tauriInvoke("system_install_pick_image");
  },
};

async function request(path, options) {
  let response;
  try {
    response = await fetch(path, { ...options, headers: { ...options?.headers, "x-rsetup-request": "1" } });
  } catch {
    const error = new Error(t("api.transport_failure"));
    error.translationKey = "api.transport_failure";
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
    error.providerMessage = body.error?.message;
    if (!error.code) {
      error.translationKey = "api.http_failure";
      error.translationParams = { status: response.status };
    }
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

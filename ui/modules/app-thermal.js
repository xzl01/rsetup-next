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
    ${clean.map((point) => `<i class="fan-curve-dot" data-fan-x="${x(point.temperatureC).toFixed(1)}" data-fan-y="${y(point.speedPercent).toFixed(1)}"></i>`).join("")}
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
  preserveToolFocus(host);
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
  applyFanCurveStyles(chart);
}

function applyFanCurveStyles(host) {
  $$("[data-fan-x]", host).forEach((dot) => {
    dot.style.setProperty("--fan-x", Number(dot.dataset.fanX) + "%");
    dot.style.setProperty("--fan-y", Number(dot.dataset.fanY) + "%");
  });
}

function bindFanCurvePanel(host) {
  applyFanCurveStyles(host);
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
  const finish = beginApply(button, "[data-fan-confirm]");
  if (!finish) return;
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
    await refreshAll({ quiet: true, invalidate: true });
  } catch (error) {
    showApplyError(result, error);
    $("span", button).textContent = t(plan.request.enabled ? "fanCurve.apply" : "fanCurve.disable");
    button.disabled = false;
  } finally {
    finish(plan.request.enabled ? "fanCurve.apply" : "fanCurve.disable");
  }
}

async function applyThermalPolicy() {
  if (!state.thermalPolicy) return;
  const button = $("[data-thermal-apply]");
  const result = $("[data-thermal-result]");
  const finish = beginApply(button, "[data-thermal-confirm]");
  if (!finish) return;
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
    await refreshAll({ quiet: true, invalidate: true });
  } catch (error) {
    const detail = displayError(error);
    showApplyError(result, error);
    $("span", button).textContent = t("thermal.apply");
    button.disabled = false;
  } finally {
    finish("thermal.apply");
  }
}

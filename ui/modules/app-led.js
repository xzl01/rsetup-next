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
  preserveToolFocus(host);
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
    <div class="led-status-orbit" aria-hidden="true"><span data-led-level="${brightness ?? 35}"></span></div>
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
  const config = { ...state.rgbLedConfig, brightness: Math.max(0, Math.min(100, Number(state.rgbLedConfig.brightness) || 0)), cycleMs: Math.max(200, Math.min(60000, Number(state.rgbLedConfig.cycleMs) || 5000)) };
  const color = rgbLedHex(config);
  return `<section class="led-control-panel" role="tabpanel">
    <div class="rgb-led-stage" data-mode="${escapeHtml(config.mode)}">
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
  $$("[data-led-level]", host).forEach((dot) => dot.style.setProperty("--led-level", Math.max(0, Math.min(100, Number(dot.dataset.ledLevel))) + "%"));
  const stage = $(".rgb-led-stage", host);
  if (stage) {
    const config = state.rgbLedConfig;
    const cycle = Math.max(200, Math.min(60000, Number(config.cycleMs) || 5000));
    stage.style.setProperty("--led-color", rgbLedHex(config));
    stage.style.setProperty("--led-level", Math.max(0, Math.min(100, Number(config.brightness) || 0)) / 100);
    stage.style.setProperty("--led-cycle", cycle + "ms");
    stage.style.setProperty("--led-breath-phase", cycle / 2 + "ms");
  }
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
  const finish = beginApply(button, "[data-led-confirm]");
  if (!finish) return;
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
    await refreshAll({ quiet: true, invalidate: true });
  } catch (error) {
    const detail = displayError(error);
    showApplyError(result, error);
    $("span", button).textContent = t(labelKey);
    button.disabled = false;
  } finally {
    finish(labelKey);
  }
}

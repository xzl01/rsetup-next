function renderHardwareTool() {
  if (!state.selectedHardware) return;
  if (state.selectedHardware !== "storage" && !state.hardwareData) return;
  const copy = hardwareToolCopy(state.selectedHardware);
  setText("[data-hardware-tool-title]", copy.title);
  setText("[data-hardware-tool-description]", copy.description);
  if (state.selectedHardware === "device-tree") renderOverlayTool();
  else if (state.selectedHardware === "gpio") renderGpioTool();
  else if (state.selectedHardware === "video") renderVideoTool();
  else if (state.selectedHardware === "thermal") renderThermalTool();
  else if (state.selectedHardware === "led") renderLedTool();
  else if (state.selectedHardware === "spi-flash") renderSpiFlashTool();
  else if (state.selectedHardware === "storage") renderStorageTool();
}

function preserveToolFocus(host) {
  const active = document.activeElement;
  if (!host?.contains(active)) return;
  const selector = "button, input, select, textarea, a[href]";
  const before = Array.from(host.querySelectorAll(selector));
  const index = before.indexOf(active);
  const identity = (node) => JSON.stringify([node.tagName, node.id, node.name,
    Object.entries(node.dataset), node.type === "radio" || node.type === "checkbox" ? node.value : null]);
  const key = identity(active);
  const start = active.selectionStart;
  const end = active.selectionEnd;
  const scroll = host.scrollTop;
  queueMicrotask(() => {
    if (active.isConnected || (document.activeElement !== document.body && document.activeElement !== active)) return;
    const after = Array.from(host.querySelectorAll(selector));
    const target = after.find((node) => identity(node) === key) || after[Math.min(index, after.length - 1)];
    if (!target || target.disabled) return;
    target.focus({ preventScroll: true });
    if (typeof start === "number" && target.setSelectionRange) {
      try { target.setSelectionRange(start, end); } catch { /* Number inputs have no text selection. */ }
    }
    host.scrollTop = scroll;
  });
}

function renderOverlayTool() {
  const data = state.hardwareData;
  const selected = new Set(state.overlaySelection);
  const host = $("[data-hardware-body]");
  preserveToolFocus(host);
  if (data.requiresAuthorization) {
    host.innerHTML = overlayReadControl(true);
    bindOverlayRead(host);
    return;
  }
  if (!data.supported || !data.overlays.length) {
    host.innerHTML = `<div class="hardware-tool-empty">${escapeHtml(hardwareReason(data.unavailableReason) || t("overlay.none"))}</div>`;
    return;
  }
  host.innerHTML = `
    ${data.bootEntry ? `<div class="tool-warning">${escapeHtml(t("overlay.kernelScope", { kernel: data.bootEntry.kernel }))}<br><code>${escapeHtml(data.bootEntry.path)}</code></div>${overlayReadControl(false, data.cached, data.collectedAt)}` : ""}
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
    state.overlayPreviewVersion = (state.overlayPreviewVersion || 0) + 1;
    renderOverlayTool();
  }));
  $("[data-overlay-preview]", host)?.addEventListener("click", previewOverlays);
  bindOverlayRead(host);
  if (!data.mutable && data.unavailableReason) {
    $("[data-overlay-plan]", host).innerHTML = `<div class="tool-warning">${escapeHtml(hardwareReason(data.unavailableReason))}</div>`;
  } else if (state.overlayPlan) {
    renderOverlayPlan();
  }
}

function overlayReadControl(required, cached = false, collectedAt = null) {
  const time = collectedAt ? new Date(collectedAt).toLocaleTimeString(i18n.getLocale()) : "";
  return `<div class="overlay-read-control">
    <p>${escapeHtml(t(required ? "overlay.readRequired" : cached ? "overlay.cached" : "overlay.savedView", { time }))}</p>
    <button class="secondary-button" type="button" data-overlay-authorize><span>${escapeHtml(t(required ? "overlay.authorize" : "overlay.refreshRead"))}</span>${icon("run")}</button>
  </div>`;
}

function bindOverlayRead(host) {
  $("[data-overlay-authorize]", host)?.addEventListener("click", authorizeOverlayRead);
}

async function authorizeOverlayRead(event) {
  const button = event.currentTarget;
  const id = state.selectedHardware;
  const version = ++state.hardwareLoadVersion;
  state.overlayPlan = null;
  state.overlayPreviewVersion = (state.overlayPreviewVersion || 0) + 1;
  $("[data-overlay-plan]")?.replaceChildren();
  button.disabled = true;
  $("span", button).textContent = t("overlay.authorizing");
  try {
    const overlays = await transport.authorizeOverlays();
    const data = id === "gpio" ? await transport.gpioStatus(gpioProfileOverride()) : overlays;
    if (state.selectedHardware !== id || state.hardwareLoadVersion !== version) return;
    state.hardwareData = data;
    if (id === "device-tree") state.overlaySelection = data.overlays.filter((item) => item.enabled).map((item) => item.id);
    renderHardwareTool();
  } catch (error) {
    if (state.selectedHardware !== id || state.hardwareLoadVersion !== version) return;
    toast(t("toast.failed"), displayError(error), true);
    button.disabled = false;
    $("span", button).textContent = t("overlay.authorize");
  }
}

async function previewOverlays() {
  const version = state.overlayPreviewVersion = (state.overlayPreviewVersion || 0) + 1;
  const loadVersion = state.hardwareLoadVersion;
  const selectedIds = [...state.overlaySelection];
  state.overlayPlan = null;
  const button = $("[data-overlay-preview]");
  button.disabled = true;
  $("span", button).textContent = t("overlay.previewing");
  try {
    const plan = await transport.planOverlays(selectedIds);
    if (state.selectedHardware !== "device-tree" || loadVersion !== state.hardwareLoadVersion || version !== state.overlayPreviewVersion) return;
    state.overlayPlan = plan;
    renderOverlayTool();
    requestAnimationFrame(() => $("[data-overlay-plan]")?.scrollIntoView({ block: "start" }));
  } catch (error) {
    if (state.selectedHardware !== "device-tree" || loadVersion !== state.hardwareLoadVersion || version !== state.overlayPreviewVersion) return;
    renderOverlayTool();
    toast(t("toast.failed"), displayError(error), true);
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
    ${plan.bootChange ? `<div class="overlay-boot-change">
      <strong>${escapeHtml(t("overlay.bootChange"))}</strong>
      <code>${escapeHtml(plan.bootChange.path)}</code>
      <dl><dt>${escapeHtml(t("overlay.before"))}</dt><dd><code>${escapeHtml(plan.bootChange.devicetreeBefore || "—")}</code><code>${escapeHtml(plan.bootChange.overlaysBefore.join("\n") || "—")}</code></dd>
      <dt>${escapeHtml(t("overlay.after"))}</dt><dd><code>${escapeHtml(plan.bootChange.devicetreeAfter)}</code><code>${escapeHtml(plan.bootChange.overlaysAfter.join("\n") || "—")}</code></dd></dl>
    </div>` : ""}
    <div class="tool-warning"><span>${escapeHtml(plan.bootChange ? t("overlay.kernelScope", { kernel: plan.bootChange.kernel }) : t("overlay.warning.reboot"))}</span><span>${escapeHtml(t("overlay.warning.kernel"))}</span></div>
    <label class="confirm-line hardware-confirm"><input type="checkbox" data-overlay-confirm ${plan.changes.length ? "" : "disabled"} /><span>${escapeHtml(t("overlay.confirm"))}</span></label>
    <button class="execute-button hardware-execute" type="button" data-overlay-apply disabled><span>${escapeHtml(t("overlay.apply"))}</span>${icon("run")}</button>
    <div class="drawer-result" data-overlay-result hidden></div>
  </section>`;
  $("[data-overlay-confirm]", host).addEventListener("change", (event) => {
    $("[data-overlay-apply]", host).disabled = !event.currentTarget.checked || !plan.changes.length;
  });
  $("[data-overlay-apply]", host).addEventListener("click", applyOverlays);
}

function beginApply(button, selector) {
  if (button.rsetupRunning) return null;
  button.rsetupRunning = true;
  const confirmation = $(selector);
  const wasDisabled = confirmation?.disabled;
  if (confirmation) confirmation.disabled = true;
  return (labelKey, safe = false) => {
    button.rsetupRunning = false;
    $("span", button).textContent = t(labelKey);
    button.disabled = !safe;
    if (confirmation) {
      confirmation.checked = false;
      confirmation.disabled = wasDisabled;
    }
  };
}

function showApplyError(result, error) {
  const canceled = error?.code === "authorization_canceled";
  result.classList.toggle("is-error", !canceled);
  result.textContent = canceled ? t("api.authorization_canceled") : displayError(error);
  toast(t(canceled ? "toast.canceled" : "toast.failed"), result.textContent, !canceled);
}

async function applyOverlays() {
  const plan = state.overlayPlan;
  if (!plan) return;
  const loadVersion = state.hardwareLoadVersion;
  const button = $("[data-overlay-apply]");
  const result = $("[data-overlay-result]");
  const finish = beginApply(button, "[data-overlay-confirm]");
  if (!finish) return;
  button.disabled = true;
  $("span", button).textContent = t("overlay.applying");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("drawer.runState");
  try {
    const applied = await transport.applyOverlays(plan.selectedIds, plan.planToken, true);
    if (state.selectedHardware !== "device-tree" || loadVersion !== state.hardwareLoadVersion) return;
    result.textContent = applied.run.synthetic ? t("sources.planned") : t("overlay.saved");
    toast(applied.run.synthetic ? t("toast.dryRun") : t("overlay.saved"), t("overlay.warning.reboot"));
    if (!applied.run.synthetic) {
      const data = applied.status || await transport.overlayStatus();
      if (state.selectedHardware !== "device-tree" || loadVersion !== state.hardwareLoadVersion) return;
      state.hardwareData = data;
      state.overlaySelection = state.hardwareData.overlays.filter((item) => item.enabled).map((item) => item.id);
      state.overlayPlan = null;
      renderOverlayTool();
    }
    await refreshAll({ quiet: true, invalidate: true });
  } catch (error) {
    if (state.selectedHardware !== "device-tree" || loadVersion !== state.hardwareLoadVersion) return;
    const detail = displayError(error);
    showApplyError(result, error);
    $("span", button).textContent = t("overlay.apply");
    button.disabled = false;
  } finally {
    finish("overlay.apply");
  }
}

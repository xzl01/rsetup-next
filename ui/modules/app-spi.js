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
  preserveToolFocus(host);
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
  const finish = beginApply(button, "[data-spi-confirm]");
  if (!finish) return;
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
    await refreshAll({ quiet: true, invalidate: true });
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
    showApplyError(result, error);
    $("span", button).textContent = t(`spiFlash.apply.${plan.request.operation}`);
    button.disabled = true;
  } finally {
    finish(`spiFlash.apply.${plan.request.operation}`);
  }
}

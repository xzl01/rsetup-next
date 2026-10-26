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
  const source = pin.functionSource === "default" ? t("gpio.source.default") : pin.sourceDetail || gpioSource(pin);
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
  // Configuration knowledge comes from the backend, not a hard-coded SBC name.
  const baselineOnly = data.configurationKnown !== true;
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
      <p>${escapeHtml(t(data.profileId ? baselineOnly ? "gpio.baselineOnly" : "gpio.profileDescription" : "gpio.genericDescription"))}</p>
      <div class="gpio-profile-facts"><b>${escapeHtml(baselineOnly ? t("gpio.configurationUnknown") : t("gpio.overlays", { count: data.configuredOverlays?.length || 0 }))}</b><b>${escapeHtml(data.layout || "40-pin")}</b></div>
    </div>
    ${data.configurationRequiresAuthorization ? overlayReadControl(true) : data.configurationKernel ? `<div class="tool-warning">${escapeHtml(t("overlay.kernelScope", { kernel: data.configurationKernel }))}</div>${overlayReadControl(false, data.configurationCached)}` : ""}
    ${data.serialConsoleDetected ? `<div class="tool-warning">${escapeHtml(t("gpio.serialWarning"))}</div>` : ""}
    ${baselineOnly ? "" : `<div class="gpio-view-note">${escapeHtml(t("gpio.currentOnly"))}</div>`}
    ${selected ? gpioPinDetail(selected) : ""}
    <div class="gpio-connectors">${connectors.map((connector, index) => gpioConnectorView(connector, pins, selectedPin, index)).join("")}</div>`;
  bindOverlayRead(host);
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
  preserveToolFocus(host);
  if (!data.supported) {
    host.innerHTML = `<div class="hardware-tool-empty">${escapeHtml(hardwareReason(data.unavailableReason) || t("hardware.unavailable"))}</div>`;
    return;
  }
  const current = $("[data-video-device]", host)?.value || data.devices[0]?.id;
  const frame = state.videoFrame;
  host.innerHTML = `
    <label class="tool-field"><span>${escapeHtml(t("video.device"))}</span><select data-video-device>${data.devices.map((device) => `<option value="${escapeHtml(device.id)}" ${device.id === current ? "selected" : ""}>${escapeHtml(device.name)} · ${escapeHtml(device.path)}</option>`).join("")}</select></label>
    <div class="camera-stage" data-camera-stage>
      ${frame ? `<img src="data:${escapeHtml(frame.mimeType)};base64,${escapeHtml(frame.base64)}" alt="${escapeHtml(t("video.title"))}" /><span>${escapeHtml(frame.synthetic ? t("video.synthetic") : t("video.captured", { time: relativeTime(frame.capturedAt) }))}</span>` : `<div>${icon("video")}<span>${escapeHtml(t("video.ready"))}</span></div>`}
    </div>
    ${!data.captureAvailable && data.unavailableReason ? `<div class="tool-warning">${escapeHtml(hardwareReason(data.unavailableReason))}</div>` : ""}
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

function applyStorageMetricStyles(host) {
  $$("[data-metric-percent]", host).forEach((bar) => {
    bar.style.setProperty("--metric-percent", `${Number(bar.dataset.metricPercent || 0)}%`);
  });
}

function renderStorageMmcCard(device) {
  const health = device.health || {};
  const typeLabel = device.cardType === "SD" ? t("storageTool.sd") : t("storageTool.emmc");
  const healthState = state.storageStale ? "unknown" : (device.healthState || "unknown");
  const badgeClass =
    healthState === "critical"
      ? "nvme-badge-critical"
      : healthState === "warning"
        ? "nvme-badge-warning"
        : healthState === "healthy"
          ? "nvme-badge-healthy"
          : "nvme-badge-unknown";

  const telemetryState = device.telemetry?.state;
  let telemetryReason = "";
  if (telemetryState === "unavailable") {
    const err = device.telemetry?.error;
    if (err) {
      const errKey =
        err.kind === "permission_denied"
          ? "storageTool.permissionDenied"
          : err.kind === "io"
            ? "storageTool.readFailed"
            : err.kind === "nvme_status"
              ? "storageTool.protocolError"
              : "storageTool.unavailable";
      telemetryReason = err.code != null ? `${t(errKey)} (${err.code})` : t(errKey);
    } else {
      telemetryReason = t("storageTool.unavailable");
    }
  }

  const healthLabel =
    state.storageStale
      ? t("storageTool.staleData")
      : healthState === "critical"
        ? t("storageTool.critical")
        : healthState === "warning"
          ? t("storageTool.warning")
          : healthState === "healthy"
            ? t("storageTool.healthy")
            : telemetryState === "unavailable"
              ? telemetryReason
              : telemetryState === "unsupported"
                ? t("storageTool.unsupported")
                : t("storageTool.unknown");

  const preEolLabel =
    health.preEolInfo === 1
      ? t("storageTool.preEolNormal")
      : health.preEolInfo === 2
        ? t("storageTool.preEolWarning")
        : health.preEolInfo === 3
          ? t("storageTool.preEolUrgent")
          : t("storageTool.preEolUndefined");

  let bottomContent = "";
  if (telemetryState === "unavailable") {
    bottomContent = `
      <div class="storage-telemetry-banner">
        <span>${escapeHtml(t("storageTool.unavailable"))}</span>:
        <b>${escapeHtml(telemetryReason)}</b>
      </div>
    `;
  } else if (telemetryState === "unsupported") {
    bottomContent = `
      <div class="storage-telemetry-banner">
        <b>${escapeHtml(t("storageTool.unsupported"))}</b>
      </div>
    `;
  } else {
    bottomContent = `
      <div class="nvme-metrics-grid">
        ${lifeBlock("storageTool.lifeA", health.lifeTimeEstAPercent)}
        ${lifeBlock("storageTool.lifeB", health.lifeTimeEstBPercent)}
      </div>

      <div class="storage-pre-eol-line">
        <span>${escapeHtml(t("storageTool.preEol"))}</span>
        <b>${escapeHtml(preEolLabel)}</b>
      </div>
    `;
  }

  function lifeBlock(labelKey, percent) {
    if (percent == null) {
      // N/A is not a percentage: the bar carries no --metric-percent and CSS
      // paints the whole track with the grey "no data" fill.
      return `
        <div class="nvme-metric-block">
          <div class="nvme-metric-header">
            <span>${escapeHtml(t(labelKey))}</span>
            <b>${escapeHtml(t("storageTool.na"))}</b>
          </div>
          <div class="nvme-metric-bar">
            <div class="nvme-metric-bar-fill is-na"></div>
          </div>
        </div>`;
    }
    const clamped = Math.min(100, Math.max(0, percent));
    const statusClass = percent > 100 ? "is-critical" : percent >= 100 ? "is-warning" : percent >= 80 ? "is-warning" : "is-normal";
    const text = percent === 100 ? "90–100%" : percent === 101 ? ">100%" : `${percent}%`;
    return `
      <div class="nvme-metric-block">
        <div class="nvme-metric-header">
          <span>${escapeHtml(t(labelKey))}</span>
          <b>${escapeHtml(text)}</b>
        </div>
        <div class="nvme-metric-bar">
          <div class="nvme-metric-bar-fill ${statusClass}" data-metric-percent="${clamped}"></div>
        </div>
      </div>`;
  }

  return `
    <section class="nvme-card storage-mmc-card">
      <div class="nvme-card-head">
        <div class="nvme-card-title">
          <strong>${escapeHtml(device.model || device.name)}<span class="storage-card-type-badge">${escapeHtml(typeLabel)}</span></strong>
          <span>${escapeHtml(device.blockPath || device.name)}</span>
        </div>
        <div class="nvme-card-badges">
          <span class="nvme-badge ${badgeClass}">${escapeHtml(healthLabel)}</span>
        </div>
      </div>

      <div class="nvme-specs-grid">
        <div class="nvme-spec-item">
          <i>${escapeHtml(t("storageTool.capacity"))}</i>
          <b>${byteUnit(device.totalBytes)}</b>
        </div>
        <div class="nvme-spec-item">
          <i>${escapeHtml(t("storageTool.serial"))}</i>
          <b>${escapeHtml(device.serial || "—")}</b>
        </div>
        <div class="nvme-spec-item">
          <i>${escapeHtml(t("storageTool.firmware"))}</i>
          <b>${escapeHtml(device.firmware || "—")}</b>
        </div>
        <div class="nvme-spec-item">
          <i>${escapeHtml(t("storageTool.manufacturer"))}</i>
          <b>${escapeHtml(device.manufacturer || "—")}</b>
        </div>
      </div>

      ${bottomContent}
    </section>
  `;
}

async function refreshStorageTool() {
  if (state.selectedHardware !== "storage") return;
  if (state.storageRefreshPromise) return state.storageRefreshPromise;
  const version = state.hardwareLoadVersion;
  state.storageRefreshing = true;
  let task;
  const current = () => state.selectedHardware === "storage"
    && state.hardwareLoadVersion === version && state.storageRefreshPromise === task;
  task = Promise.resolve().then(() => transport.storageStatus()).then((data) => {
    if (!current()) return;
    state.hardwareData = data;
    state.storageRefreshError = null;
    state.storageStale = false;
    state.storageRefreshedAt = Date.now();
  }).catch((error) => {
    if (!current()) return;
    state.storageRefreshError = error;
    state.storageStale = state.hardwareData !== null;
  }).finally(() => {
    if (!current()) return;
    state.storageRefreshing = false;
    state.storageRefreshPromise = null;
    renderHardwareTool();
  });
  state.storageRefreshPromise = task;
  renderHardwareTool();
  return task;
}

function renderStorageTool() {
  const data = state.hardwareData;
  const host = $("[data-hardware-body]");
  preserveToolFocus(host);

  const nvme = data?.nvme;
  const mmc = data?.mmc;
  const nvmeDevices = nvme?.initialized ? nvme.devices || [] : [];
  const mmcDevices = mmc?.initialized ? mmc.devices || [] : [];

  const refreshBtn = `
    <button class="secondary-button storage-refresh-button" type="button" data-storage-refresh ${state.storageRefreshing ? "disabled" : ""}>
      <svg aria-hidden="true"><use href="#icon-refresh"></use></svg>
      <span>${escapeHtml(state.storageRefreshing ? t("storageTool.refreshing") : t("storageTool.refresh"))}</span>
    </button>
  `;

  const refreshInfo = state.storageRefreshedAt
    ? t("storageTool.refreshedAt", { time: relativeTime(state.storageRefreshedAt) })
    : "";

  const toolbar = `
    <div class="storage-toolbar">
      <div class="storage-toolbar-info">
        <span>${escapeHtml(t("storageTool.title"))}</span>
        ${refreshInfo ? `<b>${escapeHtml(refreshInfo)}</b>` : ""}
      </div>
      ${refreshBtn}
    </div>
  `;

  const staleBanner = state.storageStale ? `
    <div class="storage-stale-banner" role="alert">
      ${icon("pulse")}
      <span>${escapeHtml(t("storageTool.staleData"))}</span>
    </div>
  ` : "";

  const wireEvents = () => {
    const btn = $("[data-storage-refresh]", host);
    if (btn) btn.onclick = () => void refreshStorageTool();
    const retryBtn = $("[data-storage-retry]", host);
    if (retryBtn) retryBtn.onclick = () => void refreshStorageTool();
  };

  if (!data) {
    if (state.storageRefreshError) {
      host.innerHTML = `
        ${toolbar}
        <div class="storage-retry-box">
          ${icon("pulse")}
          <strong>${escapeHtml(t("toast.failed"))}</strong>
          <span>${escapeHtml(displayError(state.storageRefreshError))}</span>
          <button class="secondary-button" type="button" data-storage-retry>${escapeHtml(t("storageTool.retry"))}</button>
        </div>
      `;
      wireEvents();
      return;
    }
    host.innerHTML = `
      ${toolbar}
      <div class="hardware-tool-loading"><span></span><span>${escapeHtml(t("hardware.loading"))}</span></div>
    `;
    wireEvents();
    return;
  }

  if (nvmeDevices.length === 0 && mmcDevices.length === 0) {
    const message =
      !(nvme?.initialized) && !(mmc?.initialized)
        ? t("storageTool.uninitialized")
        : t("storageTool.noDevices");
    host.innerHTML = `
      ${toolbar}
      ${staleBanner}
      <div class="hardware-tool-empty">${escapeHtml(message)}</div>
    `;
    wireEvents();
    return;
  }

  const parts = [];
  if (nvmeDevices.length > 0) parts.push(t("storageTool.countNvme", { count: nvmeDevices.length }));
  if (mmcDevices.length > 0) parts.push(t("storageTool.countMmc", { count: mmcDevices.length }));

  host.innerHTML = `
    ${toolbar}
    ${staleBanner}
    <div class="tool-fact-line">
      <span>${escapeHtml(t("storageTool.description"))}</span>
      <b>${escapeHtml(parts.join(" · "))}</b>
    </div>
    <div class="nvme-tool-container">
      ${nvmeDevices.map((dev) => renderNvmeDeviceCard(dev)).join("")}
      ${mmcDevices.map((dev) => renderStorageMmcCard(dev)).join("")}
    </div>
  `;
  wireEvents();
  applyStorageMetricStyles(host);
}

function renderNvmeDeviceCard(device) {
  const smart = device.smart;
  const healthState = state.storageStale ? "unknown" : (device.healthState || "unknown");
  const badgeClass =
    healthState === "critical"
      ? "nvme-badge-critical"
      : healthState === "warning"
        ? "nvme-badge-warning"
        : healthState === "healthy"
          ? "nvme-badge-healthy"
          : "nvme-badge-unknown";

  const healthLabel =
    state.storageStale
      ? t("storageTool.staleData")
      : healthState === "critical"
        ? t("nvme.critical")
        : healthState === "warning"
          ? (smart?.warningFlags?.join(", ") || t("nvme.warning"))
          : healthState === "healthy"
            ? t("nvme.healthy")
            : device.telemetry?.error?.kind === "permission_denied"
              ? t("storageTool.permissionDenied")
              : device.telemetry?.error?.kind === "nvme_status"
                ? `${t("storageTool.protocolError")} (${device.telemetry.error.code ?? 0})`
                : device.telemetry?.state === "unavailable"
                  ? t("storageTool.unavailable")
                  : t("storageTool.unknown");

  if (!smart) {
    return `
      <section class="nvme-card">
        <div class="nvme-card-head">
          <div class="nvme-card-title">
            <strong>${escapeHtml(device.model || device.name)}</strong>
            <span>${escapeHtml(device.path || device.name)}</span>
          </div>
          <div class="nvme-card-badges">
            <span class="nvme-badge ${badgeClass}">${escapeHtml(healthLabel)}</span>
          </div>
        </div>

        <div class="nvme-specs-grid">
          <div class="nvme-spec-item">
            <i>${escapeHtml(t("nvme.capacity"))}</i>
            <b>${byteUnit(device.totalBytes)}</b>
          </div>
          <div class="nvme-spec-item">
            <i>${escapeHtml(t("nvme.serial"))}</i>
            <b>${escapeHtml(device.serial || "—")}</b>
          </div>
          <div class="nvme-spec-item">
            <i>${escapeHtml(t("nvme.firmware"))}</i>
            <b>${escapeHtml(device.firmware || "—")}</b>
          </div>
          <div class="nvme-spec-item">
            <i>${escapeHtml(t("nvme.temperature"))}</i>
            <b>—</b>
          </div>
        </div>
      </section>
    `;
  }

  const temp = smart.temperatureC != null ? smart.temperatureC : null;
  const tempWarn = temp != null && temp >= 70;
  const tempCritical = temp != null && temp >= 80;
  const tempStatusClass = tempCritical ? "is-critical" : tempWarn ? "is-warning" : "is-normal";
  const tempText = temp != null ? `${formatNumber(temp, 1)} °C` : "—";

  const spare = smart.availableSparePercent != null ? smart.availableSparePercent : null;
  const spareThreshold = smart.spareThresholdPercent != null ? smart.spareThresholdPercent : 10;
  const spareWarn = spare != null && spare <= spareThreshold;
  const spareStatusClass = spareWarn ? "is-critical" : "is-cyan";

  const used = smart.percentageUsed != null ? smart.percentageUsed : null;
  const usedWarn = used != null && used >= 90;
  const usedCritical = used != null && used >= 100;
  const usedStatusClass = usedCritical ? "is-critical" : usedWarn ? "is-warning" : "is-normal";

  return `
    <section class="nvme-card">
      <div class="nvme-card-head">
        <div class="nvme-card-title">
          <strong>${escapeHtml(device.model || device.name)}</strong>
          <span>${escapeHtml(device.path || device.name)}</span>
        </div>
        <div class="nvme-card-badges">
          <span class="nvme-badge ${badgeClass}">${escapeHtml(healthLabel)}</span>
        </div>
      </div>

      <div class="nvme-specs-grid">
        <div class="nvme-spec-item">
          <i>${escapeHtml(t("nvme.capacity"))}</i>
          <b>${byteUnit(device.totalBytes)}</b>
        </div>
        <div class="nvme-spec-item">
          <i>${escapeHtml(t("nvme.serial"))}</i>
          <b>${escapeHtml(device.serial || "—")}</b>
        </div>
        <div class="nvme-spec-item">
          <i>${escapeHtml(t("nvme.firmware"))}</i>
          <b>${escapeHtml(device.firmware || "—")}</b>
        </div>
        <div class="nvme-spec-item">
          <i>${escapeHtml(t("nvme.temperature"))}</i>
          <b>${tempText}</b>
        </div>
      </div>

      <div class="nvme-metrics-grid">
        <div class="nvme-metric-block">
          <div class="nvme-metric-header">
            <span>${escapeHtml(t("nvme.spare"))}</span>
            <b>${spare != null ? `${spare}%` : "—"}</b>
          </div>
          <div class="nvme-metric-bar">
            <div class="nvme-metric-bar-fill ${spareStatusClass}" data-metric-percent="${Math.min(100, Math.max(0, spare || 0))}"></div>
          </div>
        </div>

        <div class="nvme-metric-block">
          <div class="nvme-metric-header">
            <span>${escapeHtml(t("nvme.used"))}</span>
            <b>${used != null ? `${used}%` : "—"}</b>
          </div>
          <div class="nvme-metric-bar">
            <div class="nvme-metric-bar-fill ${usedStatusClass}" data-metric-percent="${Math.min(100, Math.max(0, used || 0))}"></div>
          </div>
        </div>
      </div>

      <div class="nvme-stats-grid">
        <div class="nvme-stat-item">
          <i>${escapeHtml(t("nvme.dataRead"))}</i>
          <b>${byteUnit(smart.dataReadBytes)}</b>
        </div>
        <div class="nvme-stat-item">
          <i>${escapeHtml(t("nvme.dataWritten"))}</i>
          <b>${byteUnit(smart.dataWrittenBytes)}</b>
        </div>
        <div class="nvme-stat-item">
          <i>${escapeHtml(t("nvme.powerOnHours"))}</i>
          <b>${formatNumber(smart.powerOnHours || 0, 0)} h</b>
        </div>
        <div class="nvme-stat-item">
          <i>${escapeHtml(t("nvme.unsafeShutdowns"))}</i>
          <b>${formatNumber(smart.unsafeShutdowns || 0, 0)}</b>
        </div>
        <div class="nvme-stat-item">
          <i>${escapeHtml(t("nvme.mediaErrors"))}</i>
          <b>${formatNumber(smart.mediaErrors || 0, 0)}</b>
        </div>
        <div class="nvme-stat-item">
          <i>${escapeHtml(t("nvme.errorLogs"))}</i>
          <b>${formatNumber(smart.numErrLogEntries || 0, 0)}</b>
        </div>
      </div>
    </section>
  `;
}

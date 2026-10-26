function renderAll() {
  renderSnapshot();
  renderActions();
  renderActivity();
  renderSources();
  renderHelp();
  renderCommandResults();
  renderDebugControls();
}

function renderSnapshot() {
  const snapshot = state.snapshot;
  if (!snapshot) return;
  const { identity, metrics, storage, interfaces, services, capabilities } = snapshot;
  const memoryPercent = memoryUsagePercent(metrics);
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
  renderCpuMetric(metrics.cpuPercent);
  const memoryScale = memoryPercent == null ? 0 : Math.max(0, Math.min(100, memoryPercent));
  setText("[data-memory]", memoryPercent == null ? "—" : formatPercent(memoryPercent));
  setText("[data-temperature]", metrics.temperatureC == null ? "N/A" : `${formatNumber(metrics.temperatureC, 1)} °C`);
  setText("[data-thermal-detail]", metrics.temperatureC == null ? t("temperature.none") : metrics.temperatureC < 70 ? t("temperature.normal") : t("temperature.hot"));
  setText("[data-storage]", root ? formatPercent(rootPercent) : "N/A");
  setText("[data-storage-detail]", root ? `${byteUnit(root.usedBytes)} / ${byteUnit(root.totalBytes)}` : t("storage.unavailable"));
  setText("[data-uptime]", metrics.uptimeSeconds == null ? "—" : duration(metrics.uptimeSeconds));
  setText("[data-kernel]", identity.kernel);
  setText("[data-arch]", identity.architecture);
  setText("[data-collected]", t("updated", { time: relativeTime(snapshot.collectedAt) }));
  $("[data-memory-meter]").style.transform = `scaleX(${memoryScale / 100})`;

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

function applyStaticTranslations() {
  document.title = t("document.title");
  $("meta[name='description']")?.setAttribute("content", t("document.description"));
  $$('[data-i18n]').forEach((element) => { element.textContent = t(element.dataset.i18n); });
  $$('[data-i18n-aria]').forEach((element) => { element.setAttribute("aria-label", t(element.dataset.i18nAria)); });
  $$('[data-i18n-alt]').forEach((element) => { element.setAttribute("alt", t(element.dataset.i18nAlt)); });
  $$('[data-i18n-placeholder]').forEach((element) => { element.setAttribute("placeholder", t(element.dataset.i18nPlaceholder)); });
  setText("[data-language-label]", t("language.short"));
}

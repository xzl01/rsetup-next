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

function renderCpuMetric(value) {
  const known = typeof value === "number" && Number.isFinite(value);
  const percent = known ? Math.max(0, Math.min(100, value)) : 0;
  setText("[data-cpu]", known ? formatPercent(percent) : "—");
  $("[data-cpu-meter]").style.transform = `scaleX(${percent / 100})`;
}

function memoryUsagePercent(metrics) {
  const used = metrics.memoryUsedBytes;
  const total = metrics.memoryTotalBytes;
  return Number.isFinite(used) && Number.isFinite(total) && total > 0 && used >= 0 && used <= total
    ? used / total * 100 : null;
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

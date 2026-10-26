function providerName(providerId) {
  if (!providerId) return t("provider.unknown");
  if (providerId === "official") return t("provider.official");
  if (providerId === "mixed") return t("provider.mixed");
  return state.sources?.providers.find((provider) => provider.id === providerId)?.name || providerId;
}

function providerLocation(location) {
  const key = {
    "Global": "global",
    "China": "china",
    "Hefei, CN": "hefei",
    "Beijing, CN": "beijing",
    "Chongqing, CN": "chongqing",
    "Lanzhou, CN": "lanzhou",
    "Wuhan, CN": "wuhan",
    "Jinan, CN": "jinan",
    "Nanjing, CN": "nanjing",
    "Nanyang, CN": "nanyang",
  }[location];
  return key ? t(`location.${key}`) : location;
}

function renderSources() {
  renderSourceBenchmark();
  const source = state.sources;
  const status = $$('[data-source-status] dd');
  const select = $("[data-source-provider]");
  const preview = $("[data-source-preview]");
  if (!source) {
    status.forEach((element) => { element.textContent = t("sources.unavailable"); });
    select.disabled = true;
    preview.disabled = true;
    return;
  }
  const values = [
    `${source.distributionName} · ${source.architecture}`,
    providerName(source.currentSystemProvider),
    providerName(source.currentRadxaProvider),
    String(source.files.length),
  ];
  status.forEach((element, index) => { element.textContent = values[index] || "—"; });

  const previous = select.value;
  select.innerHTML = source.providers.map((provider) => `<option value="${escapeHtml(provider.id)}">${escapeHtml(providerName(provider.id))} · ${escapeHtml(providerLocation(provider.location))}</option>`).join("");
  const preferred = source.providers.some((provider) => provider.id === previous)
    ? previous
    : source.currentSystemProvider && source.currentSystemProvider !== "mixed"
      ? source.currentSystemProvider
      : "official";
  select.value = preferred;
  select.disabled = !source.supported;
  preview.disabled = !source.supported;
  renderProviderDetail();
  if (state.sourcePlan) renderSourcePlan(state.sourcePlan, { preserveConfirmation: true });
}

function resetSourceBenchmark() {
  state.sourceBenchmark = { version: state.sourceBenchmark.version + 1, running: false, stop: false, results: [], completed: 0, total: 0 };
}

async function benchmarkSources() {
  if (state.sourceBenchmark.running || !state.sources?.supported) return;
  const revision = state.sources.sourceRevision;
  const providers = state.sources.providers.map((provider) => provider.id);
  const version = state.sourceBenchmark.version + 1;
  const test = { version, running: true, stop: false, results: [], completed: 0, total: providers.length };
  state.sourceBenchmark = test;
  renderSourceBenchmark();
  for (const providerId of providers) {
    if (test.stop || state.sourceBenchmark.version !== version) break;
    try {
      const result = await transport.benchmarkSource(providerId);
      if (state.sourceBenchmark.version !== version) return;
      if (result.sourceRevision !== revision || result.providerId !== providerId) {
        resetSourceBenchmark();
        renderSourceBenchmark();
        toast(t("toast.failed"), t("sources.benchmarkStale"), true);
        return;
      }
      test.results.push(result);
    } catch (error) {
      if (state.sourceBenchmark.version !== version) return;
      test.results.push({ providerId, error, probes: [] });
    }
    test.completed += 1;
    renderSourceBenchmark();
  }
  if (state.sourceBenchmark.version !== version) return;
  test.running = false;
  renderSourceBenchmark();
}

function renderSourceBenchmark() {
  const test = state.sourceBenchmark;
  const button = $("[data-source-benchmark]");
  button.disabled = test.running || !state.sources?.supported;
  $("[data-source-benchmark-stop]").hidden = !test.running;
  $("[data-source-benchmark-stop]").disabled = test.stop;
  $("[data-source-benchmark-panel]").hidden = !test.running && !test.total;
  const mode = test.running ? (test.stop ? "stoppingTest" : "testing") : (test.stop ? "testStopped" : "testComplete");
  setText("[data-source-benchmark-progress]", `${state.sources?.synthetic ? `${t("sources.simulatedTest")} · ` : ""}${t(`sources.${mode}`, { done: test.completed, total: test.total })}`);
  const focusId = document.activeElement?.dataset?.benchmarkChoose;
  $("[data-source-benchmark-results]").innerHTML = test.results.map((result) => `<div class="benchmark-row">
    <button type="button" class="secondary-button benchmark-choice" data-benchmark-choose="${escapeHtml(result.providerId)}" aria-label="${escapeHtml(t("sources.chooseTested", { name: providerName(result.providerId) }))}">${escapeHtml(providerName(result.providerId))}</button>
    <div>${result.error ? `<p class="benchmark-error">${escapeHtml(benchmarkError(result.error))}</p>` : !result.probes.length ? `<p>${escapeHtml(t("sources.noTestTarget"))}</p>` : result.probes.map((probe) => {
      const label = probe.kind === "radxa" ? "Radxa" : t("sources.systemProbe");
      const measured = probe.status === "ok" && Number.isFinite(probe.latencyMs) && Number.isFinite(probe.bytesPerSecond);
      const detail = measured ? `${t("sources.latency")} ${Math.round(probe.latencyMs)} ms · ${t("sources.sampleSpeed")} ${(probe.bytesPerSecond / 1024).toFixed(1)} KiB/s` : t(`sources.testStatus.${probe.status}`);
      return `<div class="benchmark-probe" data-status="${escapeHtml(probe.status)}" title="${escapeHtml(probe.url)}"><strong>${label}</strong><span>${escapeHtml(detail)}${!measured && probe.httpStatus ? ` (HTTP ${Number(probe.httpStatus)})` : ""}</span></div>`;
    }).join("")}</div></div>`).join("");
  if (focusId) $$('[data-benchmark-choose]').find((item) => item.dataset.benchmarkChoose === focusId)?.focus({ preventScroll: true });
}

function benchmarkError(error) {
  if (error?.translationKey) return t(error.translationKey, error.translationParams);
  if (error?.code) return i18n.apiError(error.code, error.providerMessage || error.message);
  return displayError(error);
}

function renderProviderDetail() {
  const providerId = $("[data-source-provider]")?.value;
  const provider = state.sources?.providers.find((item) => item.id === providerId);
  if (!provider) return;
  setText("[data-source-provider-detail]", t("sources.providerScope", {
    location: providerLocation(provider.location),
    system: provider.systemEndpoint ? t("sources.supported") : t("sources.unchanged"),
    radxa: provider.radxaEndpoint ? t("sources.supported") : t("sources.unchanged"),
  }));
}

function clearSourcePlan() {
  state.sourcePreviewVersion += 1;
  state.sourcePlan = null;
  const host = $("[data-source-plan]");
  const confirm = $("[data-source-confirm]");
  const apply = $("[data-source-apply]");
  if (host) host.hidden = true;
  if (confirm) confirm.checked = false;
  if (apply) apply.disabled = true;
  const preview = $("[data-source-preview]");
  if (preview) {
    preview.disabled = !state.sources?.supported;
    $("span", preview).textContent = t("sources.preview");
  }
  const result = $("[data-source-result]");
  if (result) {
    result.hidden = true;
    result.classList.remove("is-error");
  }
}

function renderSourcePlan(plan, { preserveConfirmation = false } = {}) {
  const host = $("[data-source-plan]");
  const confirm = $("[data-source-confirm]");
  const confirmed = preserveConfirmation && confirm.checked;
  host.hidden = false;
  setText("[data-source-plan-title]", providerName(plan.provider.id));
  const entries = plan.changes.reduce((total, change) => total + change.replacements, 0);
  setText("[data-source-plan-count]", t("sources.changeCount", { entries, files: plan.changes.length }));
  $("[data-source-diff-list]").innerHTML = plan.changes.length ? plan.changes.map((change) => {
    const lines = change.before.flatMap((before, index) => [
      `<span class="diff-remove">- ${escapeHtml(before)}</span>`,
      `<span class="diff-add">+ ${escapeHtml(change.after[index] || "")}</span>`,
    ]).join("\n");
    return `<article class="source-diff"><header><strong>${escapeHtml(change.path)}</strong><span>${escapeHtml(t("sources.replacements", { count: change.replacements }))}</span></header><pre>${lines}</pre></article>`;
  }).join("") : `<div class="loading-row"><span></span>${escapeHtml(t("sources.noChanges"))}</div>`;
  $("[data-source-warnings]").innerHTML = plan.warnings.map((warning) => `<div class="source-warning">${escapeHtml(t(`sources.warning.${warning}`))}</div>`).join("");
  confirm.checked = confirmed;
  $("[data-source-apply]").disabled = !confirmed || !plan.changes.length;
}

async function previewSources() {
  const providerId = $("[data-source-provider]").value;
  const revision = state.sources?.sourceRevision;
  clearSourcePlan();
  const version = state.sourcePreviewVersion;
  const button = $("[data-source-preview]");
  button.disabled = true;
  $("span", button).textContent = t("sources.previewing");
  try {
    const plan = await transport.planSources(providerId);
    if (state.sourcePreviewVersion !== version || $("[data-source-provider]").value !== providerId) return;
    if (plan.provider.id !== providerId || plan.sourceRevision !== revision) {
      toast(t("toast.failed"), t("api.stale_plan"), true);
      return;
    }
    state.sourcePlan = plan;
    renderSourcePlan(plan);
    $("[data-source-plan]").scrollIntoView({ behavior: "smooth", block: "nearest" });
  } catch (error) {
    if (state.sourcePreviewVersion !== version) return;
    const detail = displayError(error);
    toast(t("toast.failed"), detail, true);
  } finally {
    if (state.sourcePreviewVersion === version) {
      $("span", button).textContent = t("sources.preview");
      button.disabled = !state.sources?.supported;
    }
  }
}

async function applySourcePlan() {
  const plan = state.sourcePlan;
  if (!plan || !$("[data-source-confirm]").checked) return;
  const button = $("[data-source-apply]");
  const result = $("[data-source-result]");
  const finish = beginApply(button, "[data-source-confirm]");
  if (!finish) return;
  button.disabled = true;
  $("span", button).textContent = t("sources.applying");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("sources.runState");
  try {
    const applied = await transport.applySources(plan.provider.id, plan.planToken, true);
    const heading = applied.run.synthetic
      ? t("sources.planned")
      : applied.rolledBack
        ? t("sources.rolledBack")
        : applied.run.status === "failed" ? t("drawer.failed") : t("sources.applied");
    result.classList.toggle("is-error", applied.run.status === "failed");
    const rawOutput = applied.run.output && !applied.run.synthetic ? `\n\n${applied.run.output}` : "";
    result.textContent = `${heading}\n${applied.run.status === "failed" ? applied.run.summary : i18n.runSummary(applied.run)}${applied.backups.length ? `\n${t("sources.backups", { count: applied.backups.length })}\n${applied.backups.join("\n")}` : ""}${rawOutput}`;
    toast(heading, i18n.runSummary(applied.run), applied.run.status === "failed");
    await refreshAll({ quiet: true, invalidate: true });
  } catch (error) {
    const detail = displayError(error);
    if (error?.code === "stale_plan" || error?.code === "plan_required") {
      state.sourcePlan = null;
      $("[data-source-confirm]").checked = false;
    }
    showApplyError(result, error);
  } finally {
    finish("sources.apply");
  }
}

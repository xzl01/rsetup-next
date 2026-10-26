function navigate(route) {
  if (!routes.some((candidate) => candidate.id === route)) return;
  state.route = route;
  $$('[data-route]').forEach((button) => {
    const active = button.dataset.route === route;
    button.classList.toggle("is-active", active);
    if (button.classList.contains("route-button")) {
      active ? button.setAttribute("aria-current", "page") : button.removeAttribute("aria-current");
      if (active) button.scrollIntoView({ block: "nearest" });
    }
  });
  $$('[data-view]').forEach((view) => view.classList.toggle("is-active", view.dataset.view === route));
  $(".workspace").scrollTo({ top: 0, behavior: "instant" });
  history.replaceState(null, "", `#${route}`);
  if (route === "install") openSystemInstallPage();
  const dialog = $("[data-command-dialog]");
  if (dialog.open) dismissDialog(dialog);
}

function renderActionDetails({ reset = false } = {}) {
  const raw = state.selectedAction;
  if (!raw) return;
  const action = i18n.action(raw);
  setText("[data-task-title]", action.title);
  setText("[data-task-description]", action.available
    ? action.description
    : `${action.description} ${t("operations.unavailableReason", { reason: action.unavailableReason || t("operations.unavailable") })}`);
  setText("[data-task-time]", `${action.estimatedSeconds}s`);
  setText("[data-task-root]", action.requiresRoot ? t("drawer.root") : t("drawer.user"));
  const risk = $("[data-task-risk]");
  risk.dataset.risk = action.risk;
  risk.textContent = riskLabel(action.risk);
  $("[data-task-steps]").innerHTML = action.steps.map((step) => `<li>${escapeHtml(step)}</li>`).join("");
  const guarded = action.risk !== "safe";
  $("[data-confirm-line]").hidden = !guarded || !action.available;
  if (reset) {
    $("[data-task-confirm]").checked = false;
    $("[data-task-execute]").disabled = !action.available || guarded;
    const result = $("[data-task-result]");
    result.hidden = true;
    result.classList.remove("is-error");
  }
}

function openAction(actionId) {
  const action = state.actions.find((candidate) => candidate.id === actionId);
  if (!action) return;
  if (!action.available) {
    toast(i18n.action(action).title, i18n.action(action).unavailableReason || t("operations.unavailable"), true);
    return;
  }
  if (action.id === "system.change-sources") {
    navigate("workflows");
    const manager = $("#source-manager");
    manager.scrollIntoView({ behavior: "smooth", block: "start" });
    window.setTimeout(() => manager.focus({ preventScroll: true }), 250);
    toast(t("sources.title"), t("sources.focused"));
    return;
  }
  state.selectedAction = action;
  state.lastInvoker = document.activeElement;
  renderActionDetails({ reset: true });
  const drawer = $("[data-task-drawer]");
  if (!drawer.open) openDialog(drawer);
  requestAnimationFrame(() => { drawer.classList.add("is-open"); $("[data-task-close]").focus(); });
}

function closeAction() {
  const drawer = $("[data-task-drawer]");
  drawer.classList.remove("is-open");
  window.setTimeout(() => {
    if (drawer.open) dismissDialog(drawer);
    state.lastInvoker?.focus?.();
    state.lastInvoker = null;
  }, 320);
  state.selectedAction = null;
}

async function executeSelectedAction() {
  const action = state.selectedAction;
  if (!action || !action.available) return;
  const button = $("[data-task-execute]");
  const result = $("[data-task-result]");
  const confirm = action.risk === "safe" || $("[data-task-confirm]").checked;
  const finish = beginApply(button, "[data-task-confirm]");
  if (!finish) return;
  button.disabled = true;
  $("span", button).textContent = t("drawer.running");
  result.hidden = false;
  result.classList.remove("is-error");
  result.textContent = t("drawer.runState");
  try {
    const run = await transport.runAction(action.id, confirm);
    const translatedAction = i18n.action(action);
    const output = run.synthetic && i18n.getLocale() === "zh-CN"
      ? `${t("run.plannedSteps")}:\n- ${translatedAction.steps.join("\n- ")}`
      : run.output;
    result.textContent = `${run.synthetic ? t("drawer.dryRun") : t("drawer.result")} / ${t(`run.status.${run.status}`)}\n${i18n.runSummary(run)}${output ? `\n\n${output}` : ""}`;
    toast(run.synthetic ? t("toast.dryRun") : t("toast.complete"), i18n.runSummary(run));
    await refreshAll({ quiet: true, invalidate: true });
  } catch (error) {
    const detail = displayError(error);
    showApplyError(result, error);
  } finally {
    finish("drawer.run", action.risk === "safe");
  }
}

function bindActionButtons() {
  $$('[data-action-id]').forEach((button) => button.addEventListener("click", () => openAction(button.dataset.actionId)));
}

function openCommands() {
  const dialog = $("[data-command-dialog]");
  openDialog(dialog);
  const input = $("[data-command-input]");
  input.value = "";
  renderCommandResults();
  requestAnimationFrame(() => input.focus());
}

function renderCommandResults() {
  const host = $("[data-command-results]");
  if (!host) return;
  const query = $("[data-command-input]")?.value.trim().toLocaleLowerCase(i18n.getLocale()) || "";
  const routeItems = routes.map((route) => ({ ...route, label: t(`route.${route.id}`), detail: t(`route.${route.id}.detail`) }))
    .filter((route) => `${route.label} ${route.detail}`.toLocaleLowerCase(i18n.getLocale()).includes(query))
    .map((route) => `<button class="command-result" type="button" data-command-route="${route.id}">${icon(route.icon)}<span><strong>${escapeHtml(route.label)}</strong><span>${escapeHtml(route.detail)}</span></span><em>${t("command.section")}</em></button>`);
  const actionItems = state.actions.map(i18n.action)
    .filter((action) => `${action.title} ${action.description} ${action.category}`.toLocaleLowerCase(i18n.getLocale()).includes(query))
    .map((action) => `<button class="command-result${action.available ? "" : " is-unavailable"}" type="button" data-command-action="${escapeHtml(action.id)}"${action.available ? "" : " disabled"}>${icon("run")}<span><strong>${escapeHtml(action.title)}</strong><span>${escapeHtml(action.available ? action.description : action.unavailableReason || t("operations.unavailable"))}</span></span><em>${escapeHtml(action.available ? riskLabel(action.risk) : t("operations.unavailable"))}</em></button>`);
  host.innerHTML = [...routeItems, ...actionItems].join("") || emptyRow(t("command.empty"));
  $$('[data-command-route]', host).forEach((button) => button.addEventListener("click", () => navigate(button.dataset.commandRoute)));
  $$('[data-command-action]', host).forEach((button) => button.addEventListener("click", () => { dismissDialog($("[data-command-dialog]")); openAction(button.dataset.commandAction); }));
}

function toast(title, detail, error = false) {
  const element = document.createElement("div");
  element.className = `toast${error ? " is-error" : ""}`;
  element.innerHTML = `<strong>${escapeHtml(title)}</strong><span>${escapeHtml(detail)}</span>`;
  $("[data-toasts]").append(element);
  window.setTimeout(() => element.remove(), 4200);
}

function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
  try { localStorage.setItem("rsetup-theme-v2", theme); } catch { /* local storage is optional */ }
}

function bindEvents() {
  $$('[data-route]').forEach((button) => button.addEventListener("click", () => navigate(button.dataset.route)));
  $$('[data-refresh]').forEach((button) => button.addEventListener("click", () => refreshAll()));
  $("[data-command-trigger]").addEventListener("click", openCommands);
  $("[data-command-input]").addEventListener("input", renderCommandResults);
  $("[data-command-dialog]").addEventListener("cancel", (event) => {
    event.preventDefault();
    dismissDialog(event.currentTarget);
  });
  $("[data-language-toggle]").addEventListener("click", () => i18n.setLocale(i18n.getLocale() === "zh-CN" ? "en" : "zh-CN"));
  $("[data-theme-toggle]").addEventListener("click", () => setTheme(document.documentElement.dataset.theme === "dark" ? "light" : "dark"));
  $$('[data-contact-open]').forEach((button) => button.addEventListener("click", openContact));
  $("[data-contact-close]").addEventListener("click", closeContact);
  $("[data-contact-dialog]").addEventListener("cancel", (event) => { event.preventDefault(); closeContact(); });
  $("[data-task-close]").addEventListener("click", closeAction);
  $("[data-task-drawer]").addEventListener("cancel", (event) => { event.preventDefault(); closeAction(); });
  $("[data-task-confirm]").addEventListener("change", (event) => {
    $("[data-task-execute]").disabled = !state.selectedAction?.available || !event.currentTarget.checked;
  });
  $("[data-task-execute]").addEventListener("click", executeSelectedAction);
  $("[data-hardware-close]").addEventListener("click", closeHardwareTool);
  $("[data-install-refresh]")?.addEventListener("click", () => void loadInstallOptions());
  $("[data-hardware-drawer]").addEventListener("cancel", (event) => { event.preventDefault(); closeHardwareTool(); });
  $("[data-source-provider]").addEventListener("change", () => { clearSourcePlan(); renderProviderDetail(); });
  $("[data-source-preview]").addEventListener("click", previewSources);
  $("[data-source-benchmark]").addEventListener("click", benchmarkSources);
  $("[data-source-benchmark-stop]").addEventListener("click", () => { state.sourceBenchmark.stop = true; renderSourceBenchmark(); });
  $("[data-source-benchmark-results]").addEventListener("click", (event) => {
    const providerId = event.target.closest("[data-benchmark-choose]")?.dataset.benchmarkChoose;
    if (!state.sources?.providers.some((provider) => provider.id === providerId)) return;
    $("[data-source-provider]").value = providerId;
    clearSourcePlan();
    renderProviderDetail();
    $("[data-source-provider]").focus();
  });
  $("[data-source-confirm]").addEventListener("change", (event) => {
    $("[data-source-apply]").disabled = !event.currentTarget.checked || !state.sourcePlan?.changes.length;
  });
  $("[data-source-apply]").addEventListener("click", applySourcePlan);
  $("[data-debug-profile]").addEventListener("change", (event) => activateDebugProfile(event.currentTarget.value));
  $("[data-debug-apply]").addEventListener("click", applyCustomDebugDevice);
  $("[data-debug-reset]").addEventListener("click", () => activateDebugProfile("provider"));
  document.addEventListener("click", (event) => {
    const menu = $("[data-debug-menu]");
    if (menu.open && !menu.contains(event.target)) menu.open = false;
  });
  window.addEventListener("rsetup:locale", () => { applyStaticTranslations(); renderAll(); renderActionDetails(); renderHardwareTool(); renderSystemInstallPage(); });
  window.addEventListener("keydown", (event) => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); openCommands(); }
    if (event.key === "Escape" && $("[data-command-dialog]").open) {
      event.preventDefault();
      dismissDialog($("[data-command-dialog]"));
    } else if (event.key === "Escape" && $("[data-task-drawer]").classList.contains("is-open")) {
      closeAction();
    } else if (event.key === "Escape" && $("[data-hardware-drawer]").classList.contains("is-open")) {
      closeHardwareTool();
    } else if (event.key === "Escape" && $("[data-debug-menu]").open) {
      $("[data-debug-menu]").open = false;
    }
  });
}

function startClock() {
  const tick = () => setText("[data-clock]", new Intl.DateTimeFormat(i18n.getLocale(), {
    hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
  }).format(new Date()));
  tick();
  window.setInterval(tick, 1000);
}

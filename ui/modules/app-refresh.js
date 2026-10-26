function openDialog(dialog) {
  if (typeof dialog.showModal === "function") dialog.showModal();
  else dialog.setAttribute("open", "");
}

function dismissDialog(dialog) {
  if (typeof dialog.close === "function") dialog.close();
  else dialog.removeAttribute("open");
}

async function refreshAll({ quiet = false, invalidate = false } = {}) {
  if (typeof refreshStorageTool === "function") void refreshStorageTool();
  state.refreshEpoch ??= 0;
  if (invalidate) {
    state.refreshEpoch += 1;
    state.refreshRequested = true;
  }
  if (!quiet) state.refreshLoud = true;
  if (state.refreshPromise) return state.refreshPromise;
  state.refreshRequested = true;
  state.refreshPromise = (async () => {
    while (state.refreshRequested) {
      state.refreshRequested = false;
      const nextQuiet = !state.refreshLoud;
      state.refreshLoud = false;
      await refreshOnce({ quiet: nextQuiet, epoch: state.refreshEpoch });
    }
  })().finally(() => {
    state.refreshPromise = null;
    state.refreshLoud = false;
  });
  return state.refreshPromise;
}

async function refreshOnce({ quiet = false, epoch = 0 } = {}) {
  const shouldResolve = !state.snapshot || !quiet;
  state.refreshing = true;
  document.body.dataset.state = "loading";
  setText("[data-footer-status]", t("status.syncing"));
  setText("[data-footer-detail]", t("status.reading"));
  try {
    const previousSourceRevision = state.sources?.sourceRevision;
    const [snapshot, actions, activity, sources] = await Promise.all([
      transport.snapshot(), transport.actions(), transport.activity(), transport.sourceStatus(),
    ]);
    if (epoch !== state.refreshEpoch) return;
    state.providerSnapshot = snapshot;
    state.snapshot = applyDebugDevice(snapshot);
    state.actions = actions;
    state.activity = activity;
    const sourceChanged = previousSourceRevision
      && previousSourceRevision !== sources.sourceRevision;
    state.sources = sources;
    if (sourceChanged) clearSourcePlan();
    if (sourceChanged) resetSourceBenchmark();
    renderAll();
    document.body.dataset.state = snapshot.synthetic ? "demo" : "live";
    if (shouldResolve) resolveSignals();
    if (!quiet) toast(t("toast.probeComplete"), snapshot.synthetic ? t("toast.demoLoaded") : t("toast.localCurrent"));
  } catch (error) {
    if (epoch !== state.refreshEpoch) return;
    const detail = displayError(error);
    document.body.dataset.state = "error";
    setText("[data-footer-status]", t("status.unavailable"));
    setText("[data-footer-detail]", detail);
    toast(t("toast.refreshFailed"), detail, true);
  } finally {
    if (epoch === state.refreshEpoch) {
      state.refreshing = false;
    }
  }
}

function resolveSignals() {
  document.body.classList.remove("is-resolving");
  void document.body.offsetWidth;
  document.body.classList.add("is-resolving");
  window.setTimeout(() => document.body.classList.remove("is-resolving"), 950);
}

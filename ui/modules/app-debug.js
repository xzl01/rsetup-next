function debugProfileById(id) {
  return debugDeviceProfiles.find((profile) => profile.id === id);
}

function gpioProfileOverride() {
  if (!state.snapshot?.synthetic || state.debugProfile === "provider") return null;
  if (state.debugProfile === "custom") return "none";
  return debugProfileById(state.debugProfile)?.pinoutProfile || "none";
}

function loadDebugState() {
  try {
    const storedProfile = localStorage.getItem("rsetup-debug-profile-v1");
    if (storedProfile === "provider" || storedProfile === "custom" || debugProfileById(storedProfile)) state.debugProfile = storedProfile;
    const storedCustom = JSON.parse(localStorage.getItem("rsetup-debug-custom-v1") || "null");
    if (storedCustom && typeof storedCustom === "object") {
      state.debugCustom = Object.fromEntries(
        Object.entries(defaultDebugCustom).map(([key, fallback]) => [
          key,
          typeof storedCustom[key] === "string" ? storedCustom[key] : fallback,
        ]),
      );
    }
  } catch { /* debug persistence is optional */ }
}

function saveDebugState() {
  try {
    localStorage.setItem("rsetup-debug-profile-v1", state.debugProfile);
    localStorage.setItem("rsetup-debug-custom-v1", JSON.stringify(state.debugCustom));
  } catch { /* debug persistence is optional */ }
}

function applyDebugDevice(snapshot) {
  if (!snapshot?.synthetic || state.debugProfile === "provider") return snapshot;
  const profile = state.debugProfile === "custom" ? state.debugCustom : debugProfileById(state.debugProfile);
  if (!profile) return snapshot;
  return {
    ...snapshot,
    identity: {
      ...snapshot.identity,
      id: `debug-${state.debugProfile}`,
      product: profile.product,
      hostname: profile.hostname,
      socVendor: profile.socVendor,
      soc: profile.soc,
      architecture: profile.architecture,
    },
  };
}

function renderDebugControls() {
  const menu = $("[data-debug-menu]");
  const synthetic = Boolean(state.providerSnapshot?.synthetic);
  menu.hidden = !synthetic;
  if (!synthetic) {
    menu.open = false;
    return;
  }
  menu.dataset.active = String(state.debugProfile !== "provider");
  const select = $("[data-debug-profile]");
  select.innerHTML = [
    `<option value="provider">${escapeHtml(t("debug.provider"))}</option>`,
    ...debugDeviceProfiles.map((profile) => `<option value="${escapeHtml(profile.id)}">${escapeHtml(profile.label)}</option>`),
    `<option value="custom">${escapeHtml(t("debug.custom"))}</option>`,
  ].join("");
  select.value = state.debugProfile;
  const custom = $("[data-debug-custom]");
  custom.hidden = state.debugProfile !== "custom";
  if (!custom.contains(document.activeElement)) {
    $("[data-debug-product]").value = state.debugCustom.product;
    $("[data-debug-vendor]").value = state.debugCustom.socVendor;
    $("[data-debug-soc]").value = state.debugCustom.soc;
    $("[data-debug-architecture]").value = state.debugCustom.architecture;
    $("[data-debug-hostname]").value = state.debugCustom.hostname;
  }
}

function activateDebugProfile(profileId) {
  state.debugProfile = profileId === "custom" || profileId === "provider" || debugProfileById(profileId) ? profileId : "provider";
  saveDebugState();
  state.snapshot = applyDebugDevice(state.providerSnapshot);
  renderSnapshot();
  renderHelp();
  renderDebugControls();
  resolveSignals();
  if (state.selectedHardware === "gpio") void openHardwareTool("gpio");
  if (state.debugProfile !== "custom") {
    toast(t("debug.title"), t("debug.changed", { device: state.snapshot.identity.product }));
  }
}

function applyCustomDebugDevice() {
  state.debugCustom = {
    product: $("[data-debug-product]").value.trim() || defaultDebugCustom.product,
    socVendor: $("[data-debug-vendor]").value.trim() || defaultDebugCustom.socVendor,
    soc: $("[data-debug-soc]").value.trim() || defaultDebugCustom.soc,
    architecture: $("[data-debug-architecture]").value.trim() || defaultDebugCustom.architecture,
    hostname: $("[data-debug-hostname]").value.trim() || defaultDebugCustom.hostname,
  };
  state.debugProfile = "custom";
  saveDebugState();
  state.snapshot = applyDebugDevice(state.providerSnapshot);
  renderSnapshot();
  renderHelp();
  renderDebugControls();
  resolveSignals();
  if (state.selectedHardware === "gpio") void openHardwareTool("gpio");
  toast(t("debug.title"), t("debug.changed", { device: state.debugCustom.product }));
}

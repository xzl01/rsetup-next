// ---------------------------------------------------------------------------
// System image install page
//
// The page mirrors the HTTP/Tauri install surface that t16 wired up:
// `GET /api/v1/storage/install/targets` -> `{ liveAllowed, targets, images }`,
// `POST .../plan` and `POST .../apply` with `planToken` + `acknowledgement`.
// Three rules are deliberately not implemented here:
//   1. The client never submits an image path, only a catalogue `imageId`.
//   2. The acknowledgement is always the string the user typed. A boolean
//      (`confirm = true`) can never satisfy the core, which compares the typed
//      device name against the observed target identity.
//   3. The disabled state below is a *display* predicate. The core re-judges the
//      acknowledgement and the plan token on every apply, so a forged state
//      cannot write anything.
// ---------------------------------------------------------------------------

function installTargetSelectable(target) {
  return Boolean(target) && !target.holdsRunningSystem && !target.mounted;
}

function installSelectedTarget() {
  const targets = state.installOptions?.targets || [];
  return targets.find((target) => target.id === state.installTargetId)
    || targets.find(installTargetSelectable)
    || null;
}

function installKindLabel(kind) {
  const keys = {
    ufs: "install.kind.ufs", emmc: "install.kind.emmc", sd: "install.kind.sd",
    nvme: "install.kind.nvme", other: "install.kind.other",
  };
  const key = keys[String(kind || "").toLowerCase()] || "install.kind.other";
  return t(key);
}

function installFormatLabel(format) {
  return t(String(format || "").toLowerCase() === "xz" ? "install.format.xz" : "install.format.raw");
}

function installWarningCopy(warning) {
  const keys = {
    writing_an_image_replaces_every_partition_on_the_target: "install.warning.replaceAll",
    the_write_cannot_be_rolled_back: "install.warning.irreversible",
    the_target_is_removable_media: "install.warning.removable",
    the_image_is_compressed_and_is_decompressed_while_writing: "install.warning.compressed",
  };
  return t(keys[warning] || "install.warning.unknown", { warning: String(warning ?? "") });
}

function installErrorCodeList() {
  return [
    "confirmation_required", "acknowledgement_mismatch", "plan_required", "stale_plan",
    "root_required", "install_conflict", "writes_running_system", "install_unsupported",
    "invalid_install_request", "authorization_canceled", "internal_error",
  ];
}

// Every failure is rendered from its stable code, so a surface failure never
// collapses into one generic sentence. `expected`/`provided` come from the plan
// and from what the user typed, exactly like the core reports them.
function installErrorCopy(failure) {
  const codes = installErrorCodeList();
  const detailCodes = [
    "confirmation_required", "acknowledgement_mismatch", "stale_plan", "install_unsupported",
    "authorization_canceled",
  ];
  const served = typeof failure?.code === "string" ? failure.code : "";
  const key = codes.includes(served) ? served : "generic";
  const expected = state.installPlan?.target?.id || "";
  const target = state.installPlan?.target?.path || "";
  const provided = String(state.installAcknowledgement ?? "").trim();
  // A failure raised by the desktop chooser, or a refusal that explains an image
  // handle, gets additional actionable copy. The stable code never changes, so a
  // reader still sees `install_unsupported` / `invalid_install_request`.
  const handleProblem = installHandleProblem(failure?.message);
  if (failure?.pick) {
    const pickKey = key === "install_unsupported" ? "install.pickFailed" : "install.pickRejected";
    return {
      code: key === "generic" ? "internal_error" : key,
      key,
      title: t(pickKey),
      detail: String(failure?.message || ""),
      hint: t(`${pickKey}.hint`),
      replan: false,
      liveDisabled: false,
      repick: true,
    };
  }
  if (handleProblem) {
    const handleKey = handleProblem === "expired" ? "install.handleExpired" : "install.handleUnknown";
    return {
      code: key === "generic" ? "internal_error" : key,
      key,
      title: t(handleKey),
      detail: String(failure?.message || ""),
      hint: t(`${handleKey}.hint`),
      replan: false,
      liveDisabled: false,
      repick: true,
    };
  }
  return {
    code: key === "generic" ? "internal_error" : key,
    key,
    title: t(`install.error.${key}`),
    detail: detailCodes.includes(key)
      ? t(`install.error.${key}.detail`, { expected, provided, target })
      : String(failure?.message || ""),
    hint: t(`install.error.${key}.hint`),
    replan: key === "stale_plan" || key === "plan_required",
    liveDisabled: key === "install_unsupported",
    repick: false,
  };
}

function installErrorFrom(error) {
  if (!error) return null;
  const code = typeof error.code === "string" ? error.code : "";
  const message = error.providerMessage || error.message || "";
  return { code, message: String(message ?? "") };
}

// The plan request carries an identifier from the fixed server-side catalogue.
// There is deliberately no `imagePath` field: the client cannot name a file.
function installPlanRequest(targetId, imageId) {
  return { targetId: String(targetId ?? ""), imageId: String(imageId ?? "") };
}

// The apply request carries the typed device name as a string. It never carries
// a boolean confirmation, and the token is opaque to this layer.
//
// The image id comes from exactly one place: the `image.id` echoed by the plan
// response. Both surfaces (`InstallSurface::request` in the HTTP server and in
// the Tauri shell) resolve the submitted catalogue id themselves and put it
// back into the plan, so the plan response is the merged source of truth. The
// page deliberately keeps no fallback to its own remembered selection: an id
// read from two places could disagree with the plan and turn a valid plan into
// a safe-but-confusing refusal. A plan without an echoed id yields an empty
// value, which the surface rejects as `invalid_install_request`.
function installApplyRequest(plan, acknowledgement) {
  return {
    targetId: String(plan?.target?.id ?? ""),
    imageId: installPlanImageId(plan),
    planToken: String(plan?.planToken ?? ""),
    acknowledgement: String(acknowledgement ?? ""),
  };
}

// The single source of the image identity: the id the plan response echoes.
// No fallback to the client's own selection, so the id shown and the id
// submitted cannot drift apart.
function installPlanImageId(plan) {
  return String(plan?.image?.id ?? "");
}

// Display-side predicate only. The core trims the typed value and compares it
// with the observed target id; this mirrors that comparison for rendering.
function installAcknowledgementReady(plan, typed) {
  const expected = plan?.target?.id ?? "";
  return expected.length > 0 && String(typed ?? "").trim() === expected;
}

function installApplyDisabled(plan = state.installPlan, typed = state.installAcknowledgement, running = state.installRunning) {
  if (!plan || running) return true;
  return !installAcknowledgementReady(plan, typed);
}

function installConfirmStateCopy(plan, typed) {
  const expected = plan?.target?.id ?? "";
  const provided = String(typed ?? "").trim();
  if (installAcknowledgementReady(plan, typed)) return t("install.confirmReady", { expected });
  if (!provided) return t("install.confirmRequired", { expected });
  return t("install.confirmMismatch", { expected, provided });
}

function installFact(labelKey, value, attribute) {
  return `<div class="install-fact"${attribute ? ` ${attribute}` : ""}><i>${escapeHtml(t(labelKey))}</i><b>${escapeHtml(value)}</b></div>`;
}

function installTargetOptionLabel(target) {
  const flags = [];
  if (target.removable) flags.push(t("install.removableYes"));
  if (target.holdsRunningSystem) flags.push(t("install.reason.runningSystem"));
  if (target.mounted) flags.push(t("install.reason.mounted", { mounts: (target.mountPoints || []).join(", ") }));
  const label = `${target.path} · ${target.model} · ${byteUnit(target.sizeBytes)} · ${installKindLabel(target.kind)}`;
  return flags.length ? `${label} · ${flags.join(" · ")}` : label;
}

function installTargetOptions(targets, selected) {
  if (!targets.length) return "";
  return targets.map((target) => {
    const selectable = installTargetSelectable(target);
    const isSelected = Boolean(selected) && target.id === selected.id;
    const label = `${installTargetOptionLabel(target)}${selectable ? "" : ` — ${t("install.notSelectable")}`}`;
    return `<option value="${escapeHtml(target.id)}"${isSelected ? " selected" : ""}${selectable ? "" : " disabled"}>${escapeHtml(label)}</option>`;
  }).join("");
}

function installImageOptions(images) {
  if (!images.length) return "";
  const picked = state.installPickedImage || {};
  return images.map((image) => {
    // A chosen file is one more entry in the same list: same value shape, same
    // single selection, only the label differs (name and size, never a path).
    const label = image.picked
      ? t("install.pickedFile", { name: picked.name || image.id, size: byteUnit(image.sizeBytes) })
      : `${image.id} · ${byteUnit(image.sizeBytes)}`;
    const pickedAttribute = image.picked ? " data-install-image-picked" : "";
    const selected = image.id === state.installImageId ? " selected" : "";
    return `<option value="${escapeHtml(image.id)}"${selected}${pickedAttribute}>${escapeHtml(label)}</option>`;
  }).join("");
}

// ---------------------------------------------------------------------------
// Image source: the desktop shell's native chooser, or the fixed catalogue.
//
// The browser only ever sees the server's catalogue of ids. The desktop shell
// additionally offers its native file chooser (`system_install_pick_image`),
// which keeps the chosen path inside the Rust process and hands the WebView an
// opaque handle plus a display name and size. Both sources are the same shape
// here: one selection value that is submitted to plan, and — for the apply
// payload — only the identity the plan response echoes back.
// ---------------------------------------------------------------------------

// The shell detection is the same one the shared IPC layer uses: the browser has
// no `window.__TAURI__.core.invoke`, so it has no chooser and gets no entry.
function installPickerAvailable() {
  return typeof tauriInvoke !== "undefined" && Boolean(tauriInvoke);
}

// Catalogue ids plus, in the desktop shell, the file the user just chose. One
// list, so the selection logic never has to ask which kind of value it holds.
// The picked entry is gated on the shell capability as well: a browser can never
// show a locally chosen file, even if some state were carried over.
function installSelectableImages(images) {
  const entries = Array.isArray(images) ? [...images] : [];
  const picked = state.installPickedImage;
  if (picked && picked.handle && installPickerAvailable()) {
    entries.push({ id: picked.handle, sizeBytes: picked.sizeBytes, picked: true });
  }
  return entries;
}

async function pickInstallImage() {
  if (!installPickerAvailable() || state.installPicking) return null;
  state.installPicking = true;
  state.installPickNotice = null;
  state.installError = null;
  renderSystemInstallPage();
  let selection = null;
  try {
    const picked = await transport.pickInstallImage();
    if (picked && !picked.cancelled && picked.handle) {
      // Display metadata only: the handle is the plan selection, the name and
      // size are what the page may show. No path ever reaches this layer.
      state.installPickedImage = {
        handle: String(picked.handle),
        name: String(picked.name || picked.handle),
        sizeBytes: Number(picked.sizeBytes || 0),
      };
      state.installImageId = state.installPickedImage.handle;
      invalidateInstallPlan();
      state.installAcknowledgement = "";
      state.installResult = null;
      selection = state.installImageId;
    } else {
      // A cancelled chooser is an answer, not an error: nothing was chosen, so
      // the previous selection stands and no result panel may appear.
      state.installPickNotice = "cancelled";
      state.installResult = null;
    }
  } catch (error) {
    state.installError = { ...installErrorFrom(error), pick: true };
    state.installResult = null;
  } finally {
    state.installPicking = false;
    renderSystemInstallPage();
  }
  return selection;
}

// Presentation-only: a refused plan/apply whose explanation is about the image
// handle gets an actionable hint next to the unchanged stable code. This decides
// copy, never identity.
function installHandleProblem(message) {
  const text = String(message ?? "");
  if (!/image handle/i.test(text)) return null;
  return /expired/i.test(text) ? "expired" : "unknown";
}

function renderInstallTargetCard(target) {
  const mounts = (target.mountPoints || []).join(", ");
  return `
    <section class="install-card" data-install-target-card data-install-target-id="${escapeHtml(target.id)}">
      <header>
        <strong>${escapeHtml(t("install.targetIdentity"))}</strong>
        <span>${escapeHtml(target.id)}</span>
      </header>
      <div class="install-facts">
        ${installFact("install.path", target.path)}
        ${installFact("install.model", target.model)}
        ${installFact("install.capacity", byteUnit(target.sizeBytes))}
        ${installFact("install.kind", installKindLabel(target.kind))}
        ${installFact("install.removable", target.removable ? t("install.removableYes") : t("install.removableNo"))}
        ${installFact("install.mountState", target.mounted && mounts ? t("install.mountedAt", { mounts }) : t("install.notMounted"))}
        ${installFact("install.runningSystem", target.holdsRunningSystem ? t("install.yes") : t("install.no"))}
      </div>
    </section>
  `;
}

function renderInstallConsequences(plan) {
  const target = plan?.target || {};
  const image = plan?.image || {};
  const destroys = plan?.destroys || [];
  const mounts = (target.mountPoints || []).join(", ");
  const warnings = Array.isArray(plan?.warnings) ? plan.warnings.map(installWarningCopy) : [];
  const partitions = destroys.length
    ? `<ul class="install-partition-list" data-install-destroys>${destroys.map((partition) => `
        <li data-install-destroy="${escapeHtml(partition.name)}">
          <b>${escapeHtml(partition.name)}</b>
          <span>${escapeHtml(t("install.partitionNumber", { number: partition.number }))}</span>
          <span>${escapeHtml(byteUnit(partition.sizeBytes))}</span>
          <span>${escapeHtml(partition.mountPoint || t("install.notMounted"))}</span>
        </li>`).join("")}</ul>`
    : `<p class="install-hint" data-install-destroys-empty>${escapeHtml(t("install.destroysNone"))}</p>`;
  const sha256 = `${image.sha256Prefix || ""}${image.prefixBytes ? ` · ${t("install.sha256Scope", { size: byteUnit(image.prefixBytes) })}` : ""}`;
  return `
    <section class="install-card" data-install-consequences>
      <header>
        <strong>${escapeHtml(t("install.consequencesTitle"))}</strong>
        <span>${escapeHtml(plan?.synthetic ? t("install.syntheticPlan") : t("install.realPlan"))}</span>
      </header>
      <div class="install-facts" data-install-plan-target>
        ${installFact("install.path", target.path)}
        ${installFact("install.model", target.model)}
        ${installFact("install.capacity", byteUnit(target.sizeBytes))}
        ${installFact("install.kind", installKindLabel(target.kind))}
        ${installFact("install.removable", target.removable ? t("install.removableYes") : t("install.removableNo"))}
        ${installFact("install.mountState", target.mounted && mounts ? t("install.mountedAt", { mounts }) : t("install.notMounted"))}
        ${installFact("install.runningSystem", target.holdsRunningSystem ? t("install.yes") : t("install.no"))}
      </div>
      <h3>${escapeHtml(t("install.destroys"))}</h3>
      ${partitions}
      <div class="install-facts" data-install-plan-image>
        ${installFact("install.imageId", installPlanImageId(plan) || "—")}
        ${installFact("install.imagePath", image.path || "")}
        ${installFact("install.format", installFormatLabel(image.format))}
        ${installFact("install.fileSize", byteUnit(image.sizeBytes))}
        ${installFact("install.writeSize", byteUnit(image.uncompressedBytes))}
        ${installFact("install.partitionTable", image.partitionTable || t("install.unknownTable"))}
        ${installFact("install.sha256", sha256)}
      </div>
      <h3>${escapeHtml(t("install.warnings"))}</h3>
      <div class="install-warnings" data-install-warnings>
        <p class="install-warning is-format" data-install-format-warning>${escapeHtml(t("install.formatNotice"))}</p>
        <p class="install-warning is-irreversible" data-install-irreversible-warning>${escapeHtml(t("install.irreversibleNotice"))}</p>
        ${warnings.map((warning) => `<p class="install-warning">${escapeHtml(warning)}</p>`).join("")}
        ${plan?.requiresRoot ? `<p class="install-warning is-admin" data-install-admin-warning>${escapeHtml(t("install.requiresRootNotice"))}</p>` : ""}
      </div>
    </section>
  `;
}

function renderInstallConfirmation(plan) {
  const expected = plan?.target?.id || "";
  const typed = state.installAcknowledgement || "";
  const ready = installAcknowledgementReady(plan, typed);
  const disabled = installApplyDisabled(plan, typed, state.installRunning);
  return `
    <section class="install-card install-confirm" data-install-confirm data-install-expected="${escapeHtml(expected)}">
      <header><strong>${escapeHtml(t("install.confirmTitle"))}</strong></header>
      <p class="install-hint">${escapeHtml(t("install.confirmHint", { expected }))}</p>
      <input class="install-ack-input" type="text" autocomplete="off" spellcheck="false" data-install-acknowledge
        aria-label="${escapeHtml(t("install.confirmTitle"))}"
        value="${escapeHtml(typed)}" placeholder="${escapeHtml(t("install.confirmPlaceholder", { expected }))}" />
      <p class="install-confirm-state ${ready ? "is-ready" : "is-disabled"}" data-install-confirm-state>${escapeHtml(installConfirmStateCopy(plan, typed))}</p>
      <button class="primary-button install-apply-button" type="button" data-install-apply${disabled ? " disabled" : ""}>${escapeHtml(state.installRunning ? t("install.applying") : t("install.apply"))}</button>
      <p class="install-hint" data-install-apply-hint>${escapeHtml(ready ? t("install.applyReadyHint") : t("install.applyDisabledHint"))}</p>
    </section>
  `;
}

function renderInstallLiveBanner(options) {
  const live = Boolean(options?.liveAllowed);
  return `
    <div class="install-live-banner ${live ? "is-live" : "is-preview"}" data-install-live-banner data-live-allowed="${live ? "true" : "false"}">
      ${icon("pulse")}
      <div>
        <strong>${escapeHtml(live ? t("install.liveAllowed") : t("install.liveDisabled"))}</strong>
        <span>${escapeHtml(live ? t("install.liveAllowedNotice") : t("install.liveDisabledNotice"))}</span>
      </div>
    </div>
  `;
}

function renderInstallErrorBox() {
  const failure = state.installError;
  if (!failure) return "";
  const copy = installErrorCopy(failure);
  return `
    <section class="install-error" role="alert" data-install-error data-install-error-code="${escapeHtml(copy.code)}">
      ${icon("pulse")}
      <div>
        <strong data-install-error-title>${escapeHtml(copy.title)}</strong>
        ${copy.detail ? `<span data-install-error-detail>${escapeHtml(copy.detail)}</span>` : ""}
        ${copy.hint ? `<span data-install-error-hint>${escapeHtml(copy.hint)}</span>` : ""}
        ${copy.replan ? `<button class="secondary-button" type="button" data-install-replan>${escapeHtml(t("install.replan"))}</button>` : ""}
        ${copy.repick && installPickerAvailable() ? `<button class="secondary-button" type="button" data-install-repick>${escapeHtml(t("install.chooseAgain"))}</button>` : ""}
      </div>
    </section>
  `;
}

function renderInstallResult(result) {
  const dryRun = Boolean(result?.dryRun);
  const verification = result?.verification || {};
  const verificationState = String(verification.state || "unsupported").toLowerCase();
  const stateKey = ["matched", "mismatched", "unsupported"].includes(verificationState) ? verificationState : "unsupported";
  const observed = Array.isArray(verification.observed) ? verification.observed.length : null;
  const expected = Array.isArray(verification.expected) ? verification.expected.length : null;
  return `
    <div class="install-facts" data-install-result-facts>
      ${installFact(dryRun ? "install.result.bytesPlanned" : "install.result.bytes", byteUnit(result?.bytesWritten))}
      ${installFact("install.result.verification", t(`install.result.verification.${stateKey}`))}
      ${observed !== null && expected !== null ? installFact("install.result.partitions", t("install.result.partitionCount", { expected, observed })) : ""}
    </div>
    ${result?.dryRun ? `<p class="install-hint" data-install-result-dry-run>${escapeHtml(t("install.result.dryRun"))}</p>` : ""}
    ${result?.plan?.synthetic ? `<p class="install-hint" data-install-result-synthetic>${escapeHtml(t("install.result.synthetic"))}</p>` : ""}
    ${verification.detail ? `<p class="install-hint" data-install-result-detail>${escapeHtml(verification.detail)}</p>` : ""}
  `;
}

function renderSystemInstallBody() {
  const options = state.installOptions;
  if (!options) {
    if (state.installLoadError) {
      const message = typeof displayError === "function" ? displayError(state.installLoadError) : String(state.installLoadError?.message || state.installLoadError);
      return `
        <div class="install-retry" data-install-load-error>
          ${icon("pulse")}
          <strong>${escapeHtml(t("install.loadFailed"))}</strong>
          <span>${escapeHtml(message)}</span>
          <button class="secondary-button" type="button" data-install-retry>${escapeHtml(t("install.retry"))}</button>
        </div>
      `;
    }
    return `<div class="hardware-tool-loading"><span></span><span>${escapeHtml(t("install.loading"))}</span></div>`;
  }

  const targets = options.targets || [];
  const images = options.images || [];
  const selectables = installSelectableImages(images);
  const picked = state.installPickedImage;
  const canPick = installPickerAvailable();
  const selectable = targets.filter(installTargetSelectable);
  const chosen = installSelectedTarget();
  const target = installTargetSelectable(chosen) ? chosen : (selectable[0] || null);
  const plan = state.installPlan;

  return `
    ${renderInstallLiveBanner(options)}
    <section class="install-picker">
      <div class="install-field">
        <label for="install-target-select">${escapeHtml(t("install.target"))}</label>
        <select id="install-target-select" data-install-targets${targets.length ? "" : " disabled"}>${installTargetOptions(targets, target)}</select>
        <p>${escapeHtml(t("install.targetHint"))}</p>
      </div>
      <div class="install-field">
        <label for="install-image-select">${escapeHtml(t("install.image"))}</label>
        <select id="install-image-select" data-install-images${selectables.length ? "" : " disabled"}>${installImageOptions(selectables)}</select>
        ${canPick ? `
          <button class="secondary-button install-pick-button" type="button" data-install-pick-image${state.installPicking ? " disabled" : ""}>
            ${icon("plus")}<span>${escapeHtml(state.installPicking ? t("install.picking") : t("install.pickImage"))}</span>
          </button>
          <p data-install-pick-hint>${escapeHtml(t("install.pickHint"))}</p>
        ` : ""}
        ${canPick && picked ? `<p class="install-picked" data-install-picked-image data-install-picked-name="${escapeHtml(picked.name)}">${escapeHtml(t("install.pickedFile", { name: picked.name, size: byteUnit(picked.sizeBytes) }))}</p>` : ""}
        <p>${escapeHtml(t("install.imageHint"))}</p>
      </div>
      <button class="primary-button" type="button" data-install-plan-button${target && selectables.length ? "" : " disabled"}>
        ${escapeHtml(state.installPlanning ? t("install.previewing") : (plan ? t("install.replan") : t("install.preview")))}
      </button>
    </section>
    ${target
      ? renderInstallTargetCard(target)
      : `<div class="hardware-tool-empty" data-install-no-target>${escapeHtml(targets.length ? t("install.noSelectableTargets") : t("install.noTargets"))}</div>`}
    ${plan ? renderInstallConsequences(plan) : `<p class="install-hint" data-install-no-plan>${escapeHtml(t("install.noPlan"))}</p>`}
    ${plan ? renderInstallConfirmation(plan) : ""}
    ${renderInstallPickNotice()}
    ${renderInstallErrorBox()}
    ${state.installResult ? `<section class="install-card install-result" data-install-result><header><strong>${escapeHtml(t(state.installResult?.dryRun ? "install.result.titleDryRun" : "install.result.title"))}</strong></header>${renderInstallResult(state.installResult)}</section>` : ""}
  `;
}

// A cancelled chooser is reported as the user's answer: a neutral note, never an
// error and never a success panel.
function renderInstallPickNotice() {
  if (state.installPickNotice !== "cancelled") return "";
  return `
    <div class="install-notice" data-install-pick-notice="cancelled" role="status">
      ${icon("pulse")}
      <div>
        <strong>${escapeHtml(t("install.pickCancelled"))}</strong>
        <span>${escapeHtml(t("install.pickCancelledHint"))}</span>
      </div>
    </div>
  `;
}

function renderSystemInstallPage() {
  const host = $("[data-install-body]");
  if (!host) return;
  if (typeof preserveToolFocus === "function") preserveToolFocus(host);
  host.innerHTML = renderSystemInstallBody();
  wireInstallEvents(host);
}

// A selection change invalidates a preview that is still in flight, so a late
// response cannot leave the page showing one device and image while the plan
// underneath it describes another.
function invalidateInstallPlan() {
  state.installPreviewVersion = (state.installPreviewVersion || 0) + 1;
  state.installPlan = null;
  state.installAcknowledgement = "";
}

function wireInstallEvents(host) {
  const targetSelect = $("[data-install-targets]", host);
  if (targetSelect) {
    targetSelect.onchange = () => {
      state.installTargetId = targetSelect.value;
      invalidateInstallPlan();
      state.installError = null;
      renderSystemInstallPage();
    };
  }
  const imageSelect = $("[data-install-images]", host);
  if (imageSelect) {
    imageSelect.onchange = () => {
      state.installImageId = imageSelect.value;
      // Choosing from the list replaces a previously chosen file.
      if (state.installPickedImage && state.installPickedImage.handle !== state.installImageId) {
        state.installPickedImage = null;
      }
      state.installPickNotice = null;
      invalidateInstallPlan();
      state.installError = null;
      renderSystemInstallPage();
    };
  }
  const pickButton = $("[data-install-pick-image]", host);
  if (pickButton) pickButton.onclick = () => void pickInstallImage();
  const planButton = $("[data-install-plan-button]", host);
  if (planButton) planButton.onclick = () => void planSystemInstall();
  const acknowledge = $("[data-install-acknowledge]", host);
  if (acknowledge) acknowledge.oninput = () => onInstallAcknowledgementInput(acknowledge.value, host);
  const applyButton = $("[data-install-apply]", host);
  if (applyButton) {
    applyButton.disabled = installApplyDisabled();
    applyButton.onclick = () => void applySystemInstall();
  }
  const replan = $("[data-install-replan]", host);
  if (replan) replan.onclick = () => void planSystemInstall();
  const repick = $("[data-install-repick]", host);
  if (repick) repick.onclick = () => void pickInstallImage();
  const retry = $("[data-install-retry]", host);
  if (retry) retry.onclick = () => void loadInstallOptions();
}

// Typing updates the disabled state in place instead of re-rendering, so the
// caret stays where the user left it. The predicate is the same one the render
// path uses, and the core still judges the value on apply.
function onInstallAcknowledgementInput(value, host) {
  state.installAcknowledgement = String(value ?? "");
  const button = host ? $("[data-install-apply]", host) : $("[data-install-apply]");
  if (button) button.disabled = installApplyDisabled();
  const confirmState = host ? $("[data-install-confirm-state]", host) : $("[data-install-confirm-state]");
  if (confirmState) {
    confirmState.textContent = installConfirmStateCopy(state.installPlan, state.installAcknowledgement);
    confirmState.classList.toggle("is-ready", installAcknowledgementReady(state.installPlan, state.installAcknowledgement));
    confirmState.classList.toggle("is-disabled", !installAcknowledgementReady(state.installPlan, state.installAcknowledgement));
  }
  return state.installAcknowledgement;
}

function openSystemInstallPage() {
  if (!state.installOptions && !state.installLoadingPromise) void loadInstallOptions();
  renderSystemInstallPage();
}

async function loadInstallOptions() {
  if (state.installLoadingPromise) return state.installLoadingPromise;
  state.installLoading = true;
  state.installLoadError = null;
  let task;
  const current = () => state.installLoadingPromise === task;
  task = Promise.resolve().then(() => transport.installTargets()).then((options) => {
    if (!current()) return;
    state.installOptions = options;
    state.installLiveAllowed = Boolean(options?.liveAllowed);
    const targets = options?.targets || [];
    const selectables = installSelectableImages(options?.images || []);
    if (!targets.some((target) => target.id === state.installTargetId)) {
      state.installTargetId = (targets.find(installTargetSelectable) || targets[0])?.id || "";
    }
    // The selection is normalised against the same combined list the page shows,
    // so a chosen file survives a catalogue refresh and a vanished catalogue id
    // does not.
    if (!selectables.some((image) => image.id === state.installImageId)) {
      state.installImageId = selectables[0]?.id || "";
      state.installPickedImage = null;
    }
    // A fresh catalogue read invalidates any plan built from the previous one,
    // and invalidates a preview still in flight: the selection may just have been
    // normalised against the new catalogue, so a late response would otherwise
    // restore a plan for a device or image the page no longer shows.
    invalidateInstallPlan();
    state.installResult = null;
    state.installError = null;
  }).catch((error) => {
    if (!current()) return;
    state.installLoadError = error;
  }).finally(() => {
    if (!current()) return;
    state.installLoading = false;
    state.installLoadingPromise = null;
    renderSystemInstallPage();
  });
  state.installLoadingPromise = task;
  renderSystemInstallPage();
  return task;
}

async function planSystemInstall() {
  const options = state.installOptions;
  if (!options || state.installPlanning) return null;
  const target = installSelectedTarget();
  const targetId = target?.id || state.installTargetId;
  const imageId = state.installImageId || (options.images || [])[0]?.id || "";
  if (!targetId || !imageId) {
    state.installError = { code: "invalid_install_request", message: t("install.noSelection") };
    renderSystemInstallPage();
    return null;
  }
  const version = (state.installPreviewVersion = (state.installPreviewVersion || 0) + 1);
  state.installPlanning = true;
  state.installError = null;
  state.installResult = null;
  renderSystemInstallPage();
  let plan = null;
  try {
    plan = await transport.installPlan(targetId, imageId);
    // The selection moved while this preview was in flight, so this plan
    // describes a device or image the page no longer shows. Drop it.
    if (version !== state.installPreviewVersion) return null;
    state.installPlan = plan;
    // A new plan invalidates any acknowledgement typed for the previous one.
    state.installAcknowledgement = "";
  } catch (error) {
    if (version !== state.installPreviewVersion) return null;
    state.installPlan = null;
    state.installError = installErrorFrom(error);
  } finally {
    state.installPlanning = false;
    renderSystemInstallPage();
  }
  return plan;
}

async function applySystemInstall() {
  const plan = state.installPlan;
  if (!plan || state.installRunning) return null;
  const typed = String(state.installAcknowledgement ?? "").trim();
  if (!installAcknowledgementReady(plan, state.installAcknowledgement)) {
    // Nothing is submitted while the typed name does not match: the button is
    // already disabled, and this guard keeps the invariant if it is bypassed.
    state.installError = { code: typed ? "acknowledgement_mismatch" : "confirmation_required", message: "" };
    renderSystemInstallPage();
    return null;
  }
  const payload = installApplyRequest(plan, state.installAcknowledgement);
  state.installRunning = true;
  state.installError = null;
  state.installResult = null;
  renderSystemInstallPage();
  let result = null;
  try {
    result = await transport.installApply(payload);
    state.installResult = result;
  } catch (error) {
    const failure = installErrorFrom(error);
    state.installError = failure;
    // A stale or missing plan must never be replayed: drop it and ask for a
    // fresh preview instead of retrying the old token.
    if (failure?.code === "stale_plan" || failure?.code === "plan_required") {
      state.installPlan = null;
      state.installAcknowledgement = "";
    }
    // A refused or expired handle is not silently dropped here: the error box
    // names the problem and offers "choose again" in the desktop shell, and the
    // apply path stays free of any second image identity.
  } finally {
    state.installRunning = false;
    renderSystemInstallPage();
  }
  if (state.installError && typeof toast === "function") {
    const copy = installErrorCopy(state.installError);
    toast(t("install.error.title"), copy.detail || copy.title, true);
  }
  return result;
}

function renderActions() {
  const actions = state.actions;
  const quick = quickActionIds
    .map((actionId) => actions.find((action) => action.id === actionId))
    .filter(Boolean);
  $("[data-operation-list]").innerHTML = quick.length ? quick.map(operationButton).join("") : emptyRow(t("operations.empty"));
  const detailed = actions.filter((action) => action.id !== "system.change-sources");
  const knownActionIds = new Set(workflowGroups.flatMap((group) => group.actions));
  const groups = workflowGroups.map((group) => ({
    id: group.id,
    actions: detailed.filter((action) => group.actions.includes(action.id)),
  }));
  const other = detailed.filter((action) => !knownActionIds.has(action.id));
  if (other.length) groups.push({ id: "other", actions: other });
  const visibleGroups = groups.filter((group) => group.actions.length);
  $("[data-workflow-table]").innerHTML = visibleGroups.length ? visibleGroups.map((group) => `
    <section class="workflow-group" data-group="${escapeHtml(group.id)}" aria-labelledby="workflow-group-${escapeHtml(group.id)}">
      <div class="workflow-group-head">
        <h2 id="workflow-group-${escapeHtml(group.id)}">${escapeHtml(t(`workflows.group.${group.id}`))}</h2>
        <span>${escapeHtml(t("workflows.group.count", { count: group.actions.length }))}</span>
      </div>
      <div class="workflow-group-list">${group.actions.map(workflowButton).join("")}</div>
    </section>`).join("") : `<div class="workflow-empty">${emptyRow(t("operations.empty"))}</div>`;
  bindActionButtons();
}

function workflowButton(raw) {
  const action = i18n.action(raw);
  const unavailable = !action.available;
  const meta = unavailable ? t("operations.unavailable") : `~${action.estimatedSeconds}s`;
  return `<button class="workflow-row${unavailable ? " is-unavailable" : ""}" type="button" data-action-id="${escapeHtml(action.id)}"${unavailable ? " disabled" : ""}${action.unavailableReason ? ` title="${escapeHtml(action.unavailableReason)}"` : ""}>
    <span class="risk-plate" data-risk="${escapeHtml(action.risk)}">${escapeHtml(riskLabel(action.risk))}</span>
    <span class="workflow-copy"><strong>${escapeHtml(action.title)}</strong>${unavailable ? `<span>${escapeHtml(action.unavailableReason || t("operations.unavailable"))}</span>` : ""}</span>
    <span class="workflow-meta">${escapeHtml(meta)}</span>${icon("run")}
  </button>`;
}

function operationButton(raw) {
  const action = i18n.action(raw);
  const unavailable = !action.available;
  const meta = unavailable ? action.unavailableReason || t("operations.unavailable") : `${action.category} · ${t("steps", { count: action.steps.length })} · ~${action.estimatedSeconds}s`;
  return `<button class="operation-row${unavailable ? " is-unavailable" : ""}" type="button" data-action-id="${escapeHtml(action.id)}"${unavailable ? " disabled" : ""}${action.unavailableReason ? ` title="${escapeHtml(action.unavailableReason)}"` : ""}>
    <span class="risk-plate" data-risk="${escapeHtml(action.risk)}">${escapeHtml(riskLabel(action.risk))}</span>
    <span class="operation-copy"><strong>${escapeHtml(action.title)}</strong><span>${escapeHtml(meta)}</span></span>${icon("run")}
  </button>`;
}

function renderActivity() {
  const activity = state.activity;
  const compact = activity.slice(0, 3);
  $("[data-event-ledger]").innerHTML = compact.length ? compact.map(eventRow).join("") : emptyListItem(t("activity.empty"));
  $("[data-activity-timeline]").innerHTML = activity.length ? activity.map(eventRow).join("") : emptyListItem(t("activity.empty"));
}

function helpResourceUrl(resource) {
  return resource.url || localizedDocsUrl(resource.path);
}

function renderHelpResource(resource) {
  return `<a class="help-resource-link" href="${escapeHtml(helpResourceUrl(resource))}" target="_blank" rel="noopener noreferrer">
    ${icon("docs")}<span><strong>${escapeHtml(t(`help.resource.${resource.kind}.title`))}</strong><span>${escapeHtml(t(`help.resource.${resource.kind}.description`))}</span></span>${icon("external")}
  </a>`;
}

function renderContactLink(channel) {
  return `<a class="contact-link" data-channel="${escapeHtml(channel.id)}" href="${escapeHtml(channel.url)}" target="_blank" rel="noopener noreferrer">
    <span class="contact-mark" aria-hidden="true">${escapeHtml(channel.mark)}</span><span class="contact-link-copy"><strong>${escapeHtml(t(`contact.channel.${channel.id}.name`))}</strong><span>${escapeHtml(t(`contact.channel.${channel.id}.detail`))}</span></span>${icon("external")}
  </a>`;
}

function renderContactLinks() {
  const host = $("[data-contact-links]");
  if (host) host.innerHTML = contactChannels.map(renderContactLink).join("");
}

function renderHelp() {
  const identity = state.snapshot?.identity;
  const profile = helpProfile(identity);
  const board = profile?.name || identity?.product || t("board.detecting");
  const soc = identity?.soc || t("provider.unknown");
  const resources = profile?.resources || genericHelpResources;

  setText("[data-help-board]", t("help.boardBadge", { board, soc }));
  setText("[data-help-library-title]", t("help.libraryTitle", { board }));
  setText("[data-help-library-description]", t(profile ? "help.libraryDescription" : "help.libraryGeneric", { board }));
  $("[data-help-links]").innerHTML = resources.map(renderHelpResource).join("");
  $("[data-help-faq]").innerHTML = helpFaqs.map((faq) => `<details class="faq-item">
    <summary><span>${escapeHtml(t(`help.faq.${faq}.question`, { board }))}</span>${icon("plus")}</summary>
    <div class="faq-answer">${escapeHtml(t(`help.faq.${faq}.answer`, { board }))}</div>
  </details>`).join("");
  renderContactLinks();
}

function openContact(event) {
  state.contactInvoker = event?.currentTarget || document.activeElement;
  renderContactLinks();
  const dialog = $("[data-contact-dialog]");
  if (!dialog.open) openDialog(dialog);
  requestAnimationFrame(() => $("[data-contact-close]").focus());
}

function closeContact() {
  const dialog = $("[data-contact-dialog]");
  if (dialog.open) dismissDialog(dialog);
  const invoker = state.contactInvoker;
  state.contactInvoker = null;
  requestAnimationFrame(() => invoker?.focus?.());
}

function eventRow(raw) {
  const event = i18n.activity(raw);
  return `<li class="event-row"><time datetime="${escapeHtml(event.at)}">${relativeTime(event.at)}</time><strong>${escapeHtml(event.title)}</strong><span>${escapeHtml(event.detail)}${event.synthetic ? ` · ${t("activity.simulated")}` : ""}</span></li>`;
}

function emptyRow(message) {
  return `<div class="loading-row"><span></span>${escapeHtml(message)}</div>`;
}

function emptyListItem(message) {
  return `<li class="loading-row"><span></span>${escapeHtml(message)}</li>`;
}

---
version: 1
slug: "ui-index-html"
primary_target: "ui/index.html"
related_targets: ["ui/styles.css","ui/app.js","ui/i18n.js"]
---

Scope: the browser control center at `ui/index.html`, shared unchanged by the Tauri desktop shell. Mode: Operate.

Audience and job: a local SBC owner or administrator must identify the board, understand its health, choose a fixed guided operation, see its risk and prerequisites, and inspect the result without feeling as though they are operating a hostile industrial console.

Content and constraints: render only live provider data or visibly labelled synthetic demonstration data. Bind the Web service to loopback by default. Dangerous procedures require confirmation and never expose arbitrary shell execution. The surface must remain keyboard operable and usable at desktop and narrow mobile widths. English and Simplified Chinese are equal presentation languages: all visible copy and accessible labels must switch together, long Chinese and English strings must reflow without clipping, and unknown provider copy must fall back safely. Locale changes must not alter API identifiers or machine-readable values.

Chosen direction: user-pinned Soft UI. A pale slate canvas holds warm white, deeply rounded surfaces with low-saturation indigo, emerald, amber, and coral signals; tinted diffused shadows replace hard frames. The first view balances a calm board-health stage with a generous guided-operations card. The memorable moment is a single soft halo that confirms the local provider has resolved. A compact language control belongs in the top utility rail and uses the same 44px touch floor and quiet field treatment as adjacent controls.

Unresolved: remote fleet topology, authentication, and additional locales are intentionally deferred.

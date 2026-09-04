---
version: 1
slug: "ui-index-html"
primary_target: "ui/index.html"
related_targets: ["ui/styles.css","ui/app.js","ui/i18n.js"]
---

Scope: the browser control center at `ui/index.html`, shared unchanged by the Tauri desktop shell. Mode: Operate.

Audience and job: a local SBC owner or administrator must identify the board, understand its health, choose a fixed guided operation, see its risk and prerequisites, and inspect the result without feeling as though they are operating a hostile industrial console.

Content and constraints: render only live provider data or visibly labelled synthetic demonstration data. Bind the Web service to loopback by default. Dangerous procedures require confirmation and never expose arbitrary shell execution. The hardware route opens focused tools for exact overlay selection, a read-only 40-pin GPIO map, single-frame camera testing, and fan/thermal governor selection. Overlay and thermal changes must show their impact and confirmation boundary; GPIO inspection must not drive lines. The Help route may match exact board product names to verified official documentation; an unknown product must fall back to Radxa-wide resources instead of guessing a board. Contact stays outside the Help content in a focused dialog. Its QQ and WeChat codes are packaged from the official Radxa Community page, carry source provenance, and link back to that maintained page; every remaining channel uses its official destination. The surface must remain keyboard operable and usable at desktop and narrow mobile widths. English and Simplified Chinese are equal presentation languages: all visible copy and accessible labels must switch together, long Chinese and English strings must reflow without clipping, and unknown provider copy must fall back safely. Locale changes must not alter API identifiers or machine-readable values.

Chosen direction: user-pinned Soft UI. A pale slate canvas holds warm white, deeply rounded surfaces with low-saturation indigo, emerald, amber, and coral signals; tinted diffused shadows replace hard frames. The first view balances a calm board-health stage with a generous guided-operations card. The memorable moment is a single soft halo that confirms the local provider has resolved. A compact language control belongs in the top utility rail and uses the same 44px touch floor and quiet field treatment as adjacent controls. Help extends this world with compact document rows and native disclosure controls. Contact remains a separate dialog: two official QR codes lead, while secondary community links stay compact and subordinate.

Direction contract:

- THESIS: Board-aware Linux administration should feel calm and legible while preserving explicit operational risk.
- OWN-WORLD: Pale slate ground, warm cloud surfaces, circular line-icon wells, grounded indigo actions, and softly tinted depth.
- STORY: Identify the current board, choose a hardware subsystem, inspect or preview the exact effect, authorize a mutation when required, and read the result.
- FIRST VIEWPORT: Keep the active view title and board context ahead of a sparse capability grid; open one focused tool in a side drawer instead of expanding every control at once.
- FORM: `seed_key=user-pinned-soft-ui`; code-led extension of the user-selected Soft UI direction, with no comp round.

Unresolved: remote fleet topology, authentication, continuous camera streaming, writable GPIO testing, non-U-Boot overlay mutation, additional board-specific documentation profiles, and additional locales are intentionally deferred.

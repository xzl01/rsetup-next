---
version: 1
slug: "ui-index-html"
primary_target: "ui/index.html"
related_targets: ["ui/styles.css","ui/app.js","ui/i18n.js"]
---

Scope: the browser control center at `ui/index.html`, shared unchanged by the Tauri desktop shell. Mode: Operate.

Audience and job: a local SBC owner or administrator must identify the board, understand its health, choose a fixed guided operation, see its risk and prerequisites, and inspect the result without feeling as though they are operating a hostile industrial console.

Content and constraints: render only live provider data or visibly labelled synthetic demonstration data. Bind the Web service to loopback by default. Dangerous procedures require confirmation and never expose arbitrary shell execution. The hardware route opens focused tools for exact overlay selection, a read-only 40-pin GPIO map, single-frame camera testing, a bilingual Thermal drawer with Kernel policy and Fan curve tabs, Linux LED trigger or RGB-pattern control, and SPI NOR boot-image management. Overlay, thermal, fan-curve, LED, and SPI changes must show their impact and confirmation boundary; GPIO inspection must not drive lines. Fan curves may address only provider-discovered `user_space` thermal zones and `pwm-fan` cooling devices. They contain 2–8 strictly temperature-increasing, speed-nondecreasing points, reach 100% no later than 90 °C, expose 0–10 °C hysteresis and 500–10,000 ms polling, and preview the resolved integer cooling state for each point. LED controls may address only provider-discovered sysfs identifiers and fixed native modes, and saved state must be restored by the new control plane rather than a legacy executor. SPI management may address only provider-discovered NOR MTD targets and trusted installed images. It must preview an immutable operation/target/image/revision plan, name the exact target and image in confirmation, back up before mutation, read back after mutation, and invalidate acknowledgement when the selection or provider revision changes. The Help route may match exact board product names to verified official documentation; an unknown product must fall back to Radxa-wide resources instead of guessing a board. Contact stays outside the Help content in a focused dialog. Its QQ and WeChat codes are packaged from the official Radxa Community page, carry source provenance, and link back to that maintained page; every remaining channel uses its official destination. The surface must remain keyboard operable and usable at desktop and narrow mobile widths. English and Simplified Chinese are equal presentation languages: all visible copy and accessible labels must switch together, long Chinese and English strings must reflow without clipping, and unknown provider copy must fall back safely. Locale changes must not alter API identifiers or machine-readable values.

The GPIO drawer consumes the GPL-3.0-or-later normalized snapshot of 20 SBC
profiles from `xzl01/pin-out` and preserves connector and physical-pin order.
It renders exactly one configured function per pin: a saved enabled Overlay
assignment takes priority, and every unmatched pin on a known SBC uses its
exact Function1 default. In the upstream snapshot, each pin's `name` is the
Function1 column and the actual default 40-pin function when no Overlay changes
that pin; rsetup normalizes it as `defaultFunction`, while Function2 and later
values remain mux candidates. A default cell shows the exact Function1 value,
such as `GPIO4_B3`, with `默认功能` / `Default function` as its compact
source, never `Pinout 默认`. An Overlay-assigned cell additionally shows its
Function1/SoC pad. Fixed power, ground, and analog pins retain their
board-defined roles and never become Overlay assignments; only unknown generic
headers remain unassigned. A matched Pinout status appears as a compact inline
badge beside the board identity. The drawer never shows GPIO-chip counts, line
numbers, direction, consumer, or kernel ownership; it never invokes `gpioget`,
implicitly requests a line, or presents mux candidates as simultaneous
functions. The saved configuration updates the map immediately, while a concise
note states that changed Overlay selections become active only after reboot.
The synthetic debug device switch refreshes identity and GPIO profile together;
a monotonic hardware-load version discards stale responses from older device
selections.

Fan-curve status is provider truth, not form state. `status.config` establishes whether a curve is persisted and `status.active` distinguishes running from saved-but-stopped; the zone/device selectors and curve controls remain a draft until the exact immutable `plan.request`, revision-bound token, and resolved cooling states are explicitly confirmed. Every edit clears preview and acknowledgement. Live mutation takes the exclusive cross-process lock before final revalidation; the revision includes `previous_policy`. Sensor failure, fatal daemon error, SIGTERM, and systemd `ExecStop` force maximum cooling, while deliberate disable restores the preserved kernel governor.

SPI preview ownership is session-bound. A monotonic preview-session version advances on operation or selector changes, drawer reopen or close, and each new preview, so only the latest request for the still-open SPI drawer may publish success or failure. Apply uses the immutable `plan.request`; a stale or mismatched response clears acknowledgement and requires a fresh preview.

Fan-curve preview ownership follows the same fresh-request discipline. A preview-session version invalidates older responses and any edit clears the plan; Apply uses the returned `plan.request` rather than reconstructing values from the current selectors.

Chosen direction: user-pinned Soft UI. A pale slate canvas holds warm white, deeply rounded surfaces with low-saturation indigo, emerald, amber, and coral signals; tinted diffused shadows replace hard frames. The first view balances a calm board-health stage with a generous guided-operations card. The memorable moment is a single soft halo that confirms the local provider has resolved. A compact language control belongs in the top utility rail and uses the same 44px touch floor and quiet field treatment as adjacent controls. Help extends this world with compact document rows and native disclosure controls. Contact remains a separate dialog: two official QR codes lead, while secondary community links stay compact and subordinate.

Direction contract:

- THESIS: Board-aware Linux administration should feel calm and legible while preserving explicit operational risk.
- OWN-WORLD: Pale slate ground, warm cloud surfaces, circular line-icon wells, grounded indigo actions, and softly tinted depth.
- STORY: Identify the current board, choose a hardware subsystem, inspect or preview the exact effect, authorize a mutation when required, and read the result.
- FIRST VIEWPORT: Keep the active view title and board context ahead of a sparse capability grid; open one focused tool in a side drawer instead of expanding every control at once.
- FORM: `seed_key=user-pinned-soft-ui`; code-led extension of the user-selected Soft UI direction, with no comp round.

Unresolved: remote fleet topology, authentication, continuous camera streaming,
writable GPIO testing, real-SBC validation of Overlay naming and mux-mode
coverage across the 20 imported profiles, non-U-Boot overlay mutation,
non-Rockchip direct-MTD boot-image layouts, additional board-specific
documentation profiles, and additional locales are intentionally deferred.

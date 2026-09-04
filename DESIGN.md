---
name: "rsetup Soft Control Center"
description: "A calm, tactile Soft UI for deliberate local SBC control."
colors:
  ground: "#f4f6fa"
  surface: "#ffffff"
  field: "#eef1f7"
  field-strong: "#e5e9f3"
  line: "#dce2ed"
  line-dim: "#e9edf4"
  ink: "#293246"
  ink-muted: "#596579"
  indigo: "#5962a5"
  indigo-hover: "#4f5797"
  on-indigo: "#ffffff"
  indigo-soft: "#eceefa"
  cyan: "#4e8199"
  cyan-soft: "#e6f1f4"
  emerald: "#32715d"
  emerald-soft: "#e5f2ed"
  amber: "#87561f"
  amber-soft: "#f8eddf"
  coral: "#a83f50"
  coral-soft: "#fae9ec"
  dark-ground: "#202432"
  dark-surface: "#2a2f41"
  dark-field: "#343a4e"
  dark-field-strong: "#3c435a"
  dark-line: "#454c62"
  dark-line-dim: "#383e52"
  dark-ink: "#f4f5fb"
  dark-ink-muted: "#b8c0d2"
  dark-indigo: "#aeb5ea"
  dark-indigo-hover: "#bec4f2"
  dark-on-indigo: "#272b3b"
  dark-indigo-soft: "#393f5a"
  dark-cyan: "#8ec3d7"
  dark-cyan-soft: "#304654"
  dark-emerald: "#83c0aa"
  dark-emerald-soft: "#304b45"
  dark-amber: "#ddb078"
  dark-amber-soft: "#4d4034"
  dark-coral: "#e89aa5"
  dark-coral-soft: "#533b45"
typography:
  display:
    fontFamily: "Rsetup Sans, PingFang SC, Microsoft YaHei, Noto Sans CJK SC, sans-serif"
    fontSize: "clamp(32px, 4vw, 48px)"
    fontWeight: 800
    lineHeight: 1.08
    letterSpacing: "-0.035em"
  headline:
    fontFamily: "Rsetup Sans, PingFang SC, Microsoft YaHei, Noto Sans CJK SC, sans-serif"
    fontSize: "30px"
    fontWeight: 800
    lineHeight: 1.1
    letterSpacing: "-0.03em"
  title:
    fontFamily: "Rsetup Sans, PingFang SC, Microsoft YaHei, Noto Sans CJK SC, sans-serif"
    fontSize: "18px"
    fontWeight: 800
    letterSpacing: "-0.02em"
  body:
    fontFamily: "Rsetup Sans, PingFang SC, Microsoft YaHei, Noto Sans CJK SC, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.55
    letterSpacing: "normal"
  utility:
    fontFamily: "Rsetup Sans, PingFang SC, Microsoft YaHei, Noto Sans CJK SC, sans-serif"
    fontSize: "12px"
    fontWeight: 800
    lineHeight: 1
    letterSpacing: "0.02em"
  label:
    fontFamily: "Rsetup Mono, monospace"
    fontSize: "11px"
    fontWeight: 500
    lineHeight: 1.3
    letterSpacing: "normal"
rounded:
  inset: "8px"
  compact: "9px"
  badge: "12px"
  control-tight: "13px"
  icon-well: "15px"
  control: "16px"
  nav-mobile: "17px"
  control-large: "18px"
  row: "20px"
  bar: "22px"
  dock-mobile: "23px"
  surface: "24px"
  dialog: "28px"
  cloud: "30px"
  circular: "999px"
spacing:
  micro: "4px"
  compact: "8px"
  control: "12px"
  regular: "16px"
  row: "20px"
  panel: "24px"
  generous: "28px"
  page: "32px"
components:
  button-primary:
    backgroundColor: "{colors.indigo}"
    textColor: "{colors.on-indigo}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "11px 22px"
    height: "44px"
  button-icon:
    backgroundColor: "{colors.indigo-soft}"
    textColor: "{colors.indigo}"
    rounded: "{rounded.icon-well}"
    size: "44px"
  language-control:
    backgroundColor: "{colors.field}"
    textColor: "{colors.ink-muted}"
    typography: "{typography.utility}"
    rounded: "{rounded.control}"
    padding: "0 13px"
    height: "44px"
  navigation-route:
    backgroundColor: "transparent"
    textColor: "{colors.ink-muted}"
    typography: "{typography.label}"
    rounded: "{rounded.row}"
    padding: "7px 4px"
    height: "72px"
  card-cloud:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    rounded: "{rounded.cloud}"
    padding: "28px"
  chip-risk-safe:
    backgroundColor: "{colors.emerald-soft}"
    textColor: "{colors.emerald}"
    typography: "{typography.label}"
    rounded: "{rounded.badge}"
    padding: "5px 8px"
  chip-risk-guarded:
    backgroundColor: "{colors.amber-soft}"
    textColor: "{colors.amber}"
    typography: "{typography.label}"
    rounded: "{rounded.badge}"
    padding: "5px 8px"
  chip-risk-critical:
    backgroundColor: "{colors.coral-soft}"
    textColor: "{colors.coral}"
    typography: "{typography.label}"
    rounded: "{rounded.badge}"
    padding: "5px 8px"
  input-command:
    backgroundColor: "{colors.field}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.row}"
    padding: "0 16px"
    height: "70px"
  guarded-change-plan:
    backgroundColor: "{colors.field}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.surface}"
    padding: "20px"
---

# Design System: rsetup Soft Control Center

## Overview

**Creative North Star: "The Calm Control Cloud"**

rsetup turns a serious local SBC control plane into a calm, tactile workspace. A pale slate ground supports warm white cloud surfaces, low-saturation indigo actions, softly colored feedback, and generous breathing room. The result should feel approachable and touchable while remaining explicit about provider truth, synthetic data, privilege, risk, and execution state.

The system uses rounded geometry and diffused depth to create an orderly operating environment rather than an austere instrument wall. The board-health stage remains the visual anchor; guided procedures, capability state, and recent activity sit in clearly separated surfaces that reduce cognitive pressure. Circular icon wells provide a recurring visual signature without turning the interface playful.

The light theme is the primary expression. The optional dark theme remaps the same semantic roles to deep blue-slate surfaces and lifted pastel signals; it does not change hierarchy, interaction, or risk meaning.

English and Simplified Chinese are equal presentation locales. Locale changes replace static, provider-derived, action, result, and accessibility copy as one presentation update while leaving the control plane and its machine contracts unchanged.

**Key Characteristics:**

- Pale slate canvas and warm white cloud surfaces.
- Low-saturation indigo for actions, selection, focus, and the resolved board state.
- Deep rounding, circular icon wells, generous spacing, and diffused tinted shadows.
- A board-first overview with a generous guided-operations companion surface.
- Focused hardware drawers for overlay selection, a 20-profile GPIO header map
  that shows each Overlay assignment or exact default Function1 value, camera test frames,
  kernel thermal policy or temperature-driven fan curves, LED trigger or RGB-pattern control, and
  reviewable SPI boot-flash changes without turning the hardware route into a
  dense configuration table.
- A contained RGB preview whose color, brightness, and cycle affect only the
  rendered light, never the surrounding drawer or controls.
- Hover lift, pillow-like press feedback, and a broad halo for keyboard focus.
- Risk and provider truth communicated through words and structure as well as color.
- Equal English and Simplified Chinese presentation with locale-neutral control-plane semantics.

Hardware cards are launch surfaces, not miniature settings panels. Their tool
drawers preserve the same cloud surface and field depth while giving pin maps,
camera frames, policy choices, LED patterns, SPI image layouts, and exact change
plans enough width. Read-only signals remain visually distinct from confirmed
root mutations.

## Colors

The default palette is light, cool, and low-chroma. Indigo provides the product voice; emerald, amber, coral, and cyan remain semantic accents rather than decoration.

### Primary

- **Grounded Indigo** (`indigo`): primary actions, active navigation, focus context, board illustration, and resolved-state emphasis. The darker hover partner reinforces movement without increasing saturation.
- **Indigo Mist** (`indigo-soft`): selected navigation, icon wells, selection, and quiet control backgrounds.

### Secondary

- **Measured Cyan** (`cyan`): memory and secondary telemetry that must remain distinct from success.
- **Confirmed Emerald** (`emerald`): healthy, available, safe, and successful state.
- **Guard Amber** (`amber`): synthetic provider state, guarded operations, caution, and acknowledgement boundaries.

### Tertiary

- **Critical Coral** (`coral`): high or critical risk, failure, and destructive boundaries.

### Neutral

- **Pale Slate Ground** (`ground`): application canvas and the space between floating surfaces.
- **Warm Cloud** (`surface`): top bar, navigation dock, status bar, panels, drawers, and dialogs.
- **Soft Field** (`field`) and **Pressed Field** (`field-strong`): inset telemetry stages, operation rows, inputs, and disabled controls.
- **Soft Line** (`line`) and **Quiet Line** (`line-dim`): low-emphasis dividers and meter tracks.
- **Slate Ink** (`ink`) and **Muted Slate Ink** (`ink-muted`): primary and supporting text.

**The Semantic Color Rule.** Safe, guarded, critical, demo, running, success, and error states always include readable language or a structural cue; hue only reinforces the meaning.

**The Quiet Indigo Rule.** Indigo identifies action, selection, focus, or provider resolution. It is not a general-purpose decorative wash.

**The Theme Role Rule.** The optional dark theme changes values, never semantic roles or risk meaning. Components consume the shared semantic variables.

## Typography

**Display and Body Font:** Rsetup Sans, implemented with Open Sans Regular and ExtraBold assets, followed for Chinese glyphs by PingFang SC, Microsoft YaHei, Noto Sans CJK SC, and the generic sans-serif fallback.

**Label/Mono Font:** Rsetup Mono, implemented with Source Code Pro Medium.

**Character:** Open Sans keeps the interface friendly, plain-spoken, and highly legible while its heavy weight gives headings confidence. Source Code Pro distinguishes machine-derived values, timestamps, risk labels, shortcuts, and provider state without making the whole interface feel terminal-like.

### Hierarchy

- **Display** (800, `clamp(32px, 4vw, 48px)`, 1.08): one dominant title for the current operating view.
- **Headline** (800, 30px, 1.1): drawer titles and rare local emphasis.
- **Title** (800, 18px): cloud-surface headings and subsystem names.
- **Body** (400, 14px, 1.55 in English; 1.62 in Simplified Chinese): descriptions, guidance, procedures, and supporting context; long lines stay near 68 characters where space permits.
- **Label** (500, 11px): machine-derived values, timestamps, telemetry labels, shortcuts, risk chips, and compact metadata.
- **Utility** (800, 12px, 1, `0.02em`): the compact top-rail language control.

**The Human First Rule.** Use the sans face for actions and explanations; reserve mono for values or compact labels that genuinely come from the control plane.

**The One Display Moment Rule.** Each view gets one large title. Repeating display scale inside panels collapses the hierarchy.

**The CJK Coverage Rule.** Chinese presentation inherits the Rsetup Sans-first CJK fallback stack and 1.62 body line-height; missing glyph boxes and Latin-density line spacing are release defects.

## Layout

Desktop uses three floating anchors: a 72px top bar, a 104px route dock at left, and a 44px status bar at the bottom. The workspace is centered to a 1240px maximum and uses generous 24–32px gaps. The overview uses an asymmetric two-column grid: board health receives roughly twice the width of guided operations, while capabilities and activity flow beneath.

At 1080px the route dock narrows and lower overview bands span the grid. At 860px the overview becomes one column and operation rows form a two-column group. At 780px the route dock becomes a six-item, 64px bottom dock; the workspace and status bar account for it, and dense tables collapse secondary metadata. At 560px, including the reviewed 390px viewport, the board-health stage grows vertically, operation and capability lists become single-column, panel gutters contract to 20px, and the status bar remains above the bottom dock. Hardware drawers remain internally scrollable; LED facts and RGB adjustment fields stack into one column, while SPI target facts and image components also stack without collapsing the install/erase switch or its touch targets.

The GPIO drawer keeps the selected-pin evidence card ahead of the physical
header and preserves each connector's paired pin order at narrow widths. It
never replaces the 40-pin spatial relationship with a flat function list.

The package-mirror manager uses a two-part desktop body: a compact 2×2 status summary and a slightly wider trusted-provider workflow. At 560px and below, the workflow deliberately moves before the passive status cells so the next safe action remains visible; the status summary stays 2×2 and the reviewable change plan follows both.

English and Chinese strings reflow without clipping at desktop and at the 390px mobile viewport. Shipping evidence is `.impeccable/review/i18n-zh-desktop.png`, `.impeccable/review/i18n-zh-mobile.png`, `.impeccable/review/i18n-zh-user.png`, and `.impeccable/review/i18n-en-desktop.png`.

The package-mirror extension is verified at the same responsive boundaries in `.impeccable/review/sources-zh-user-1450.png`, `.impeccable/review/sources-en-desktop-1280.png`, and `.impeccable/review/sources-zh-mobile-390.png`.

The SPI boot-flash extension is verified in `.impeccable/review/spi-desktop.png`, `.impeccable/review/spi-user.png`, and `.impeccable/review/spi-mobile.png`.

The bilingual fan-curve extension is verified at desktop, user-width, plan, and narrow-mobile boundaries in `.impeccable/review/fan-curve-desktop.png`, `.impeccable/review/fan-curve-user.png`, `.impeccable/review/fan-curve-plan-desktop.png`, and `.impeccable/review/fan-curve-mobile.png`.

Interactive controls use a minimum 44px target. The theme and command icon controls are 44px by 44px, text actions have a 44px minimum height, the primary action is at least 44px tall, and mobile navigation cells are at least 52px tall.

**The Board-First Rule.** Responsive reflow may move supporting information, but board identity and health remain the first and strongest content block.

**The Floating-Rail Accounting Rule.** Workspace, drawer, status, and toast insets must account for the top bar and current navigation dock so content and feedback never hide behind fixed chrome.

**The Touch Floor Rule.** Every interactive target is at least 44px in one dimension, with enough spacing to avoid accidental adjacent activation.

## Elevation & Depth

Depth is soft, tinted, and structural. Warm cloud surfaces float above the pale slate canvas; inset fields sit one tonal step lower. Routine cards use broad cool shadows, while controls use tighter indigo-tinted shadows. Hover increases lift by 2px and slightly broadens the shadow. Press removes that lift and adds an inset shadow so controls compress like a pillow.

### Shadow Vocabulary

- **Soft Float** (`0 12px 32px rgba(77, 89, 130, 0.10)`): status surfaces and light utility elevation.
- **Cloud Lift** (`0 20px 48px rgba(77, 89, 130, 0.12)`): navigation, top chrome, and primary panels.
- **Hover Lift** (`0 24px 54px rgba(89, 98, 165, 0.18)`): interactive rows and cloud surfaces on hover.
- **Control Lift** (`0 8px 20px rgba(89, 98, 165, 0.13)`): compact icon wells and controls.
- **Overlay Lift** (`0 28px 80px rgba(48, 57, 91, 0.24)`): drawers, command dialog, and toasts.
- **Provider Resolve Halo** (`0 24px 56px rgba(89, 98, 165, 0.26)` at peak): a finite 820ms confirmation that freshly collected provider state has resolved.
- **Focus Halo** (`0 0 0 5px rgba(89, 98, 165, 0.48)`): keyboard-visible focus around controls in the light theme, remapped for dark surfaces.

**The Soft Depth Rule.** Use tinted diffusion and tonal layering. Hard frames and high-contrast drop shadows do not belong in the default Soft UI.

**The Finite Halo Rule.** The provider halo runs once after the initial resolution or a user-requested refresh, then returns to the normal cloud shadow. It never pulses at rest or repeats during quiet polling.

## Shapes

The form language is deeply rounded and touch-oriented. Major panels and the board core use 30px cloud corners; plot fields use 24px; dialogs and docks use 23–30px; operation rows and command fields use 18–20px; compact controls use 13–16px. Meters, status dots, capability rings, and icon wells are circular.

Borders are largely absent from floating surfaces. Tonal separation, shadows, and spacing carry hierarchy; quiet one-pixel dividers appear only inside dense data rows. Icon wells use a soft fill to give line icons a stable, touchable home.

**The Cloud Silhouette Rule.** New primary surfaces should read as calm, generous volumes with large radii and internal space, not cramped boxes.

**The Circular Well Rule.** Use circular wells for compact line icons and state signals; keep their color semantic and their size subordinate to text.

## Components

### Buttons

- **Shape:** compact actions use 13–16px radii; the primary execution action uses an 18px radius and a 52px minimum height.
- **Primary:** indigo fill, white text, 11px by 22px padding, and a cool control shadow; reserved for refresh and explicit execution.
- **Hover / Focus:** hover lifts 2px and deepens the shadow; keyboard focus uses the 5px halo. Active state scales to 0.95–0.97 and switches to an inset press shadow.
- **Icon / Text:** icon buttons are 44px by 44px with soft-indigo wells. Text buttons retain a 44px minimum height and reveal a soft fill plus underline on hover.
- **Disabled:** execution controls use the stronger field tone, muted text, no shadow, and a not-allowed cursor; confirmation requirements remain explicit nearby.

### Chips

- **Style:** risk chips use soft semantic fills, matching dark text, 12px corners, compact mono labels, and at least 68px width.
- **State:** safe maps to emerald, guarded to amber, and high or critical to coral. Every chip retains its written risk label.

### Cards / Containers

- **Corner Style:** 30px for primary cloud surfaces, 24px for inset stages, and 28–30px for overlays.
- **Background:** warm cloud surfaces sit on pale slate; inner stages and rows use soft fields.
- **Shadow Strategy:** cloud lift at rest, hover lift only where the entire surface is interactive.
- **Border:** none on floating surfaces; quiet dividers inside dense data structures only.
- **Internal Padding:** 24–28px on desktop, contracting to 20px on narrow mobile.

### Inputs / Fields

- **Style:** command search uses a 70px soft field with 20px corners, a transparent text input, muted placeholder, and indigo caret.
- **Focus:** focus belongs to the containing field and renders as a 5px halo so the whole control reads as active.
- **Error / Disabled:** errors use coral-soft background with coral text; disabled execution uses the stronger neutral field and explicit state copy.

### Navigation

The desktop route dock is a floating 104px cloud with 72px rounded route cells. Each icon sits in a circular field well. Active state uses an indigo-mist cell and an indigo-filled icon well; hover adds slight lift. At 780px and below, the same six routes become a 64px bottom dock with 52px cells and retain labels, active state, and touch spacing.

### Language Control

The top utility rail contains a compact language control with a 44px height, 54px minimum width, 16px corners, quiet field background, muted ink, and 13px horizontal padding. Its short label always names the alternate locale: `中文` while English is active and `EN` while Simplified Chinese is active. It uses the same hover lift, focus halo, and compressed active response as adjacent controls.

Switching locale updates visible static copy, provider-derived and action copy, dynamic results, placeholders, accessible labels, document language, metadata, and title together. The initial locale is auto-detected, a user choice is persisted, and command search matches localized route and action copy. Risk meaning, confirmation gates, API and action IDs, and machine-readable values remain invariant.

**The Presentation-only Locale Rule.** Locale may change every human-facing string and its reflow, but never the operation selected, its risk, its confirmation boundary, or any stable machine identifier or value.

### Board Health Stage

The signature stage places a warm, rounded device card at the center of a soft field with four stable telemetry positions. Two large, translucent tinted circles add low-contrast atmosphere without behaving as live signals. Fresh provider data triggers one finite scale-and-shadow resolve on the device card; measured meters animate to their values.

### Guided Operation Drawer

An operation opens in a rounded right-edge modal drawer with risk chip, impact description, time and privilege facts, numbered procedure, acknowledgement gate, result field, and explicit execution button. Non-safe procedures remain disabled until acknowledged. Closing restores focus to the invoking control.

**The Procedure-not-Button Rule.** Mutating work must expose impact, prerequisites, risk, confirmation, running state, and result; never reduce a recovery-sensitive operation to an isolated action.

### GPIO Pinout Drawer

Hardware > GPIO combines the detected SBC identity with a normalized catalog
of 20 pinout profiles. The profile summary, selected-pin evidence card, and
paired physical header belong to one read-only drawer. Each physical pin shows
one current function rather than a parallel list of possible mux functions.
When the detected SBC matches a known profile, that status sits beside the
board name as a compact inline badge rather than becoming a separate banner.

Enabled Overlay files under `/boot` are the configuration source for peripheral
functions. The resolver matches the function family, controller, and mux mode;
an Overlay assignment takes priority, while every unmatched pin on a known SBC
uses its exact default Function1 value. In the `xzl01/pin-out` source, each
pin's `name` is the Function1 column and the actual default 40-pin function
when no Overlay changes that pin; rsetup normalizes that value as
`defaultFunction`, while Function2 and later entries remain mux candidates.
Default cells show the exact Function1 value, such as `GPIO4_B3`, with the
compact source label `默认功能` / `Default function`, never `Pinout 默认`.
Overlay-assigned cells additionally retain their Function1/SoC-pad evidence.
Fixed power, ground, and analog pins retain their board-defined roles and never
become Overlay assignments. Unknown generic headers remain unassigned. The
drawer does not show GPIO-chip counts, line numbers, direction, consumer, or
runtime kernel ownership, and it never invokes `gpioget` or requests a line.

The map reflects the saved configuration immediately. Its concise status note
keeps the activation boundary explicit: a newly saved Overlay selection takes
effect only after the SBC reboots.

Changing the synthetic debug device refreshes the visible SBC identity and its
pinout profile together. A monotonic hardware-load version rejects older
in-flight responses so rapid device changes cannot restore a stale header.
Overlay naming and mux-mode coverage across real SBC packages remains a
physical validation boundary.

**The Overlay-before-default Rule.** Show the saved Overlay assignment when it matches a pin and retain that cell's Function1/SoC pad; otherwise show the known board's exact `defaultFunction` value with `默认功能` / `Default function`, leaving only unknown generic headers unassigned.

**The Fixed-pin Rule.** Power, ground, and analog pins keep their board-defined functions; Overlay semantics apply only to pins that can be reassigned.

**The Function1-source Rule.** `xzl01/pin-out` pin `name` is Function1 and normalizes to `defaultFunction`; Function2 and later values are mux candidates, not simultaneous or fallback defaults.

**The Saved-before-reboot Rule.** Update the map from the saved Overlay selection immediately, but state that a changed selection becomes active only after reboot.

**The Latest-device-response Rule.** A GPIO response may render only when it still belongs to the selected debug device and the latest hardware-load version.

### Fan Curve Drawer

Hardware > Thermal uses one bilingual right-edge drawer with two explicit tabs: Kernel policy and Fan curve. The curve tab keeps the current provider status, temperature-to-speed chart, temperature-zone and `pwm-fan` selectors, editable points, cool-down hysteresis, sampling interval, review action, warnings, acknowledgement, and result in one scrollable procedure. At narrow widths the targets and point controls stack without hiding status or reducing the 44px touch floor.

The provider response is authoritative for saved and running state: `status.config` establishes whether a curve is persisted, and `status.active` distinguishes a running service from a saved but stopped service. Selector, point, hysteresis, and polling edits remain draft-only; they update the chart and next request but never relabel the persisted curve or running fan as though the draft were active.

A curve contains 2–8 points with strictly increasing temperature and nondecreasing speed, and must reach 100% at or below 90 °C. Hysteresis is 0–10 °C and polling is 500–10,000 ms. Review resolves every percentage to the selected cooling device's integer state and returns one immutable request/revision/token plan. Any draft edit clears review and acknowledgement; Apply sends that exact `plan.request` only after explicit confirmation.

The plan revision includes the persisted previous thermal policy. Live apply acquires the exclusive cross-process lock before the final status revalidation and plan-token comparison. While the curve runs, sensor failure, a fatal daemon error, SIGTERM shutdown, and systemd `ExecStop` force the selected cooling device to its maximum state. Disabling the curve stops the service and restores the preserved kernel governor.

**The Provider-Truth Fan Status Rule.** Saved, running, and stopped labels come from `status.config` and `status.active`; draft selectors and curve edits never impersonate applied fan state.

**The Exact Fan Curve Plan Rule.** Apply or disable may execute only the immutable request, revision, resolved cooling-state mapping, and token that were reviewed and explicitly confirmed; any edit or stale provider state requires a new preview.

**The Maximum-Cooling Exit Rule.** Sensor failure, fatal daemon failure, SIGTERM, and systemd `ExecStop` must attempt maximum cooling before control exits; disabling deliberately restores the preserved kernel governor instead.

### LED Control Drawer

LED is an independent hardware capability card that opens the shared right-edge hardware drawer. A two-tab switch separates standalone status LEDs from RGB groups and disables a branch when the provider reports no matching hardware. The status view keeps the selected sysfs identifier, current trigger, boot-saved trigger, and measured brightness visible; the RGB view keeps the detected group, solid/breathing/rainbow mode, color, brightness, and cycle controls together.

The RGB preview stays inside a neutral 24px soft field. Color, brightness, and cycle update only its light core and diffuse glow; the drawer, labels, fields, and confirmation boundary retain their semantic colors. Changing the LED, trigger, RGB group, mode, color, brightness, or cycle clears the prior confirmation and disables Apply. The confirmation copy explicitly requests administrator authorization, states that the change is applied now, and states that the configuration is restored at boot.

Once confirmed, Apply becomes a solid indigo action. Execution replaces its label with a running state and exposes result feedback; success clears confirmation, refreshes provider state, and reports the outcome through the result/toast treatment, while failure stays in the result field and allows a deliberate retry.

**The Contained LED Preview Rule.** RGB color, intensity, and timing belong to the rendered light only; they never recolor the control surface or stand in for written state.

**The Fresh LED Authorization Rule.** Any LED configuration change invalidates the previous acknowledgement. Apply becomes available only after the current configuration has its own explicit administrator confirmation, and success clears it again.

### SPI Boot Flash Drawer

SPI boot flash is an independent hardware capability card that opens the shared right-edge hardware drawer. A two-tab switch separates writing an installed boot image from erasing the detected SPI NOR. The write branch exposes only provider-discovered NOR targets and trusted installed images, then shows device path, capacity, erase-block size, image layout, component names, and exact offsets before any action is available.

Preview is mandatory. Its returned plan binds operation, target, image, and provider revision as one immutable authorization object. A monotonic preview-session version advances whenever the selection, drawer session, or preview changes, so only the newest preview request for the still-open SPI drawer may commit success or failure. Confirmation names the exact target and image shown in that plan; changing a selector clears the plan and acknowledgement, and Apply sends the immutable `plan.request` rather than reconstructing one from current controls. A missing, mismatched, or stale plan returns the drawer to Preview instead of preserving acknowledgement.

The plan states that the current flash is backed up before change and that the result is read back for verification. Power-loss and incompatible-image risks remain in a warm guarded field directly above the confirmation. Apply stays disabled until that exact plan is acknowledged. At 560px and below, the device facts and image metadata become one column while the drawer remains vertically scrollable.

**The Exact SPI Plan Rule.** SPI write or erase may execute only the exact operation, target, image, and revision shown in the acknowledged plan; every mismatch or stale response invalidates the plan and requires a new preview.

### Package Mirror Manager

Package mirrors use one large cloud surface rather than a generic settings form. The header pairs a line-icon well with the title and plain-language effect, while written Guarded and Administrator authorization badges keep risk and privilege visible before interaction. A compact status summary reports distribution, current system source, current Radxa source, and managed-file count without competing with the actionable workflow.

The workflow begins with a trusted-provider selector in a 22px soft field. Provider location and separate system/Radxa support are written beneath the selector; choosing a provider never mutates the board. Preview opens a 24px change-plan field headed by the provider name and an entry/file count. Each affected path appears in its own 18px warm-cloud block with mono before/after lines, using coral for removals and emerald for additions. Third-party repositories that are outside the recognized scope remain untouched.

The acknowledgement line is bound to the exact previewed plan and source revision. Quiet refreshes may preserve its checked state only while that revision is unchanged; a changed or stale plan clears the acknowledgement and requires a fresh preview. The indigo Apply action remains disabled until the current plan contains changes and its confirmation is checked. On narrow mobile, the selector and Preview action appear before the passive 2×2 status summary.

**The Reviewed Plan Rule.** A privileged change may execute only the exact per-file plan the user reviewed; stale state invalidates confirmation instead of silently rebuilding or substituting a plan.

**The Purposeful Motion Rule.** Use 200–300ms transitions for lift, press, and spatial entry, and the soft ease for finite resolution. Reduced-motion preference collapses animations and transitions to 1ms.

## Do's and Don'ts

### Do:

- **Do** keep board identity and current state legible before presenting operations.
- **Do** use pale slate, warm cloud surfaces, grounded indigo, large radii, and softly tinted depth as the default visual grammar.
- **Do** preserve visible focus, the skip link, native dialog behavior, focus return, reduced-motion handling, and 44px touch targets.
- **Do** pair every risk, provider, synthetic, running, success, and error state with readable language and structural treatment.
- **Do** keep synthetic telemetry visibly labelled and mutating synthetic actions explicitly dry-run.
- **Do** preserve the same semantic roles when offering the optional dark theme.
- **Do** switch static, dynamic, and accessibility copy together, including document language and title, and search the localized route and action catalog.
- **Do** verify both locales against the four committed i18n evidence screenshots, including the 390px mobile viewport.
- **Do** preserve physical pin order and show exactly one configured function with a text-labelled source: Overlay assignment first, exact `defaultFunction` with `默认功能` / `Default function` otherwise; retain Function1/SoC-pad evidence on Overlay cells.
- **Do** keep the matched Pinout state as a compact inline badge beside the board identity, and keep fixed power, ground, and analog roles visually fixed.
- **Do** identify the map as saved Overlay configuration and keep the reboot-to-activate boundary visible.
- **Do** keep fan-curve provider status separate from draft selectors, show resolved integer cooling states, and require a fresh exact-plan confirmation after every curve edit.
- **Do** keep LED status and RGB controls separated, keep the RGB response contained to the rendered light, and require fresh administrator confirmation for the current configuration.
- **Do** show the SPI NOR target, trusted installed image, component offsets, backup, readback, and boot risks before requesting confirmation for the exact previewed plan.
- **Do** show trusted-provider scope, affected paths, exact before/after lines, privilege, and confirmation before applying a package-mirror plan.
- **Do** keep the actionable mirror workflow before the passive status summary at 560px and below.

### Don't:

- **Don't** reintroduce hostile industrial-console styling, neon accents, etched frames, or dense instrument decoration.
- **Don't** remove generous spacing or deep rounding to fit more controls above the fold.
- **Don't** use permanent glow, pulsing ambience, or repeated provider resolution during background polling.
- **Don't** make dangerous operations feel safe through friendly styling; retain explicit risk, impact, privilege, acknowledgement, and result copy.
- **Don't** encode safe, guarded, critical, demo, or error states through color alone.
- **Don't** hide primary board state, touch target size, or risk information to simplify narrow layouts.
- **Don't** translate or remap API IDs, action IDs, risk meaning, confirmation gates, or machine-readable values.
- **Don't** label a Function1 default as `Pinout 默认`, substitute a Function2+ mux candidate for `defaultFunction`, display mux candidates as simultaneous functions, expose kernel GPIO line metadata, or imply that a newly saved Overlay is active before reboot.
- **Don't** label fixed power, ground, or analog pins as Overlay-assigned or unassigned.
- **Don't** let an older GPIO response replace the header for a newly selected debug device.
- **Don't** label a draft fan target or edited curve as active, bypass the 2–8-point monotonic and 100%-by-90 °C limits, or reuse acknowledgement after the plan changes.
- **Don't** tint the LED drawer with the chosen RGB color or leave its Apply action enabled after a configuration edit or successful execution.
- **Don't** reuse an SPI acknowledgement after any operation, target, image, or provider-revision change, or execute values reconstructed from controls instead of the reviewed plan.
- **Don't** preserve acknowledgement when the package-source revision changes, or apply a plan other than the one shown in the current diff.

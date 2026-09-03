# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

Delegated and confirmed: a Rust control core shared by a `clap` CLI, a
`ratatui` TUI, an `axum` local API/Web application, and an optional Tauri
desktop shell. The browser and desktop GUI use the same frontend.

## Users

The primary user is an SBC owner, developer, or administrator working directly
on one Linux-based board. They need to understand the device, change system and
hardware settings, and recover from common configuration problems without
memorizing board-specific commands.

Fleet operators are a future audience. The first release is deliberately
local-first, while the core API and device identity model must leave room for
remote nodes later.

## Product Purpose

rsetup is a one-stop SBC control center. It unifies device observability,
configuration, maintenance, hardware controls, and guided operations behind
three coordinated entry points: CLI, TUI, and GUI.

Success means that a user can identify the board's state, find the right
operation, understand its risk, run it deliberately, and see an auditable
result from whichever interface fits their environment.

## Positioning

Unlike a generic Linux dashboard, rsetup understands SBC-specific concepts such
as device-tree overlays, boot media and bootloaders, GPIO and LEDs, thermal
policy, board identity, and recovery-sensitive operations. Every interface is a
view over the same typed operation catalog rather than a separate collection of
scripts.

## Operating Context

- Local Debian or Ubuntu SBC installations, including headless systems.
- Interactive terminal sessions, SSH sessions, a local browser, or a desktop
  WebView.
- Some operations require root privileges and may affect bootability.
- Hardware availability differs by board, kernel, enabled overlays, and attached
  peripherals.

## Capabilities and Constraints

- The first implementation manages one local device and binds its Web service
  to loopback by default.
- CLI, TUI, browser GUI, and Tauri desktop shell share one Rust domain model and
  action registry.
- English and Simplified Chinese are first-class presentation languages across
  CLI, TUI, browser GUI, and Tauri. Machine-readable JSON and HTTP payloads stay
  locale-neutral and preserve stable identifiers.
- Existing rsetup Bash functionality is migration input only. The new control
  plane must not invoke or require the legacy command at runtime; incompatible
  internal changes are allowed.
- Read-only inspection must work without elevated privileges where Linux
  exposes the data.
- Mutating and recovery-sensitive actions must show impact, prerequisites, and
  confirmation requirements. Arbitrary shell execution is not a product
  feature.
- Development on non-SBC hosts uses explicit synthetic data and dry-run action
  results. Synthetic state must never be presented as real hardware evidence.
- Remote multi-device management is an open future decision, not part of the
  first delivery.

## Brand Commitments

Preserve the `rsetup` name and Radxa/SBC technical character. The voice should
be concise, calm, operational, and explicit about risk. Do not imply hardware
support or successful execution without current evidence.

The control center uses a Soft UI visual world: light slate grounds, warm white
surfaces, low-saturation indigo as the primary action color, generous spacing,
large rounded forms, and softly tinted shadows. It should feel approachable and
touchable without making dangerous operations look harmless; risk continues to
be stated with explicit labels, impact copy, and confirmation gates.

## Evidence on Hand

The upstream repository contains production Bash modules for system updates,
bootloader and boot-media operations, overlays, networking, GPIO/LED controls,
users, localization, services, and common tasks. It also contains the existing
`librtui` terminal interface and Debian packaging.

No approved user research, fleet requirements, production telemetry, or visual
identity assets beyond the existing rsetup/Radxa association are present.

## Product Principles

- One typed control plane, many interfaces.
- Make the device state legible before offering actions.
- Treat dangerous operations as guided procedures, not buttons.
- Adapt to detected capabilities instead of showing impossible controls.
- Separate synthetic, planned, running, successful, and failed states clearly.

## Accessibility & Inclusion

The Web GUI must be keyboard navigable, retain clear focus states, respect
reduced-motion and contrast preferences, and avoid using color as the only
status signal. English and Simplified Chinese layouts must tolerate text
expansion at desktop and narrow mobile widths, update document language and
accessible labels, and fall back safely for unknown provider copy. CLI and TUI
output must remain understandable without true-color support; Chinese output
requires a terminal font with CJK glyph coverage.

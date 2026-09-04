# rsetup next

[![CI](https://github.com/xzl01/rsetup-next/actions/workflows/ci.yml/badge.svg)](https://github.com/xzl01/rsetup-next/actions/workflows/ci.yml)

One control plane for a Linux SBC, available as a scriptable CLI, an interactive
TUI, a loopback Web console, and an optional Tauri desktop application.

This branch is an alpha architecture built from the original
[`radxa-pkg/rsetup`](https://github.com/radxa-pkg/rsetup). The new Rust control
plane owns probing, policy, and action execution directly; it does not invoke or
require the legacy `rsetup` command at runtime.

## What is working

| Surface | Entry | Purpose |
| --- | --- | --- |
| CLI | `rsetup-next status`, `actions`, `run`, `sources`, `hardware`, `doctor` | automation and recovery-friendly output, including JSON |
| TUI | `rsetup-next tui` | keyboard-operated local mission control |
| Web GUI | `rsetup-next serve` | responsive browser control center and JSON API |
| Desktop GUI | `apps/desktop` | Tauri shell using the same static UI and `rsetup-core` |

The core currently probes board identity, operating system, memory, load,
temperature, uptime, storage, network interfaces, selected services, and
hardware capability signals. Its guided operation catalog covers inspection,
system update, the complete OpenSSH and Docker service lifecycles, network
recovery, root-filesystem expansion, reversible sleep policy, and reboot.
Operations that do not apply to the current system remain visible with a reason
instead of failing only after execution starts.

APT source management is implemented as a guided workflow across the CLI, TUI,
Web, and Tauri surfaces. It detects both traditional `.list` files and Deb822
`.sources` files, limits replacements to known Debian, Ubuntu, and Radxa
endpoints, leaves third-party repositories untouched, and previews every
affected line before confirmation. Live application creates timestamped
backups, writes atomically, runs `apt-get update`, and automatically restores
the previous files if the refresh fails.

The native hardware manager now covers seven migrated workflows:

- Device-tree overlays are listed from the managed U-Boot directory, checked
  for declared resource conflicts and package requirements, previewed with an
  exact revision-bound token, and renamed transactionally before
  `u-boot-update`. Changes apply after reboot.
- The 40-pin GPIO header is a read-only map backed by 20 normalized SBC
  profiles from `xzl01/pin-out`. Each physical pin shows exactly one configured
  function: the saved enabled Overlay assignment takes priority, otherwise a
  known SBC shows the Pinout `Function1` value used without an Overlay. Unknown
  generic headers remain unassigned.
  The drawer omits GPIO-chip, line, direction, consumer, and kernel-ownership
  metadata. Saved Overlay changes are shown immediately and marked as requiring
  a reboot to activate; the status path never invokes `gpioget` or requests a
  GPIO line.
- Video4Linux devices can capture a bounded single-frame webcam test through
  `ffmpeg`; device IDs are enumerated and validated rather than accepted as
  arbitrary paths.
- Thermal zones and cooling devices are inspected directly from sysfs. The
  original thermal-governor choice is preserved, including the
  `pwm-fan`/`power_allocator` incompatibility check, and the selected policy
  is restored at boot by a native systemd unit.
- A temperature-driven `pwm-fan` curve accepts 2–8 points with increasing
  temperatures and nondecreasing speeds, requires 100% cooling by 90 °C,
  exposes bounded hysteresis and polling, and previews the resolved integer
  cooling states before an exact plan can be confirmed. Disabling it restores
  the preserved kernel governor.
- Linux LED class devices expose validated status-light triggers and supported
  RGB groups. Saved trigger and RGB state is restored at boot by the new
  control plane.
- SPI boot flash management detects NOR MTD targets and trusted installed
  Rockchip U-Boot layouts. Write and erase operations require an exact
  revision-bound plan, create a root-only backup, verify readback, and attempt
  restoration if the operation fails.

## Safety model

rsetup separates observation from mutation:

- On a non-Linux development host it automatically uses labelled synthetic SBC
  telemetry.
- Mutating actions are dry-run by default on every host.
- Live execution requires Linux, root where declared, and an explicit
  `RSETUP_EXECUTION=live` or `--live-execution` opt-in.
- Guarded, high-risk, and critical actions require confirmation.
- The HTTP API accepts action identifiers from a fixed catalog; it does not
  expose arbitrary shell execution.
- The Web server binds to `127.0.0.1:8788` by default.
- Fan-curve apply and disable acquire an exclusive cross-process lock before
  final status revalidation. The exact request and a revision that includes
  the persisted `previous_policy` are bound to the preview token and require
  explicit confirmation.
- A missing temperature sample, fatal controller error, SIGTERM, or systemd
  `ExecStop` forces the configured cooling device to its maximum state.

Synthetic snapshots demonstrate the interface. They are not evidence that a
physical board or peripheral has been tested.

When a synthetic device is changed from the debug menu, the visible SBC
identity and GPIO profile refresh together. A monotonically increasing load
version prevents an older in-flight GPIO response from replacing the newer
device selection.

## Build and run

```bash
cargo build --workspace
cargo test --workspace

# Inspect the current host. macOS automatically returns demo telemetry.
cargo run -p rsetup-next -- status

# Explicit demonstration mode.
cargo run -p rsetup-next -- --demo tui
cargo run -p rsetup-next -- --demo serve
```

Open `http://127.0.0.1:8788` after starting the server.

Common CLI operations:

```bash
rsetup-next --demo status --json
rsetup-next --demo actions --json
rsetup-next --demo run system.inspect
rsetup-next --demo run system.update --confirm
rsetup-next --demo sources status
rsetup-next --demo sources plan cqu
rsetup-next --demo sources apply cqu --plan-token PLAN_TOKEN_FROM_PREVIEW --confirm
rsetup-next --demo hardware overlays status --json
rsetup-next --demo hardware overlays plan --enable rk3588-uart2-m0.dtbo
rsetup-next --demo hardware gpio --json
rsetup-next --demo hardware leds status --json
rsetup-next --demo hardware spi-flash status --json
rsetup-next --demo hardware spi-flash plan install mtd0 --image rock-5b-rk3588:rockchip-rk35
rsetup-next --demo hardware video status
rsetup-next --demo hardware video capture video0 --output camera.svg
rsetup-next --demo hardware thermal status
rsetup-next --demo hardware thermal set step_wise --confirm
rsetup-next --demo hardware thermal fan-curve status --json
rsetup-next --demo hardware thermal fan-curve plan --zone thermal_zone0 \
  --device cooling_device0 --point 40:20 --point 55:45 \
  --point 70:75 --point 82:100 --json
rsetup-next doctor
```

On a Linux board, inspect and preview as an ordinary user. A direct CLI session
can still elevate explicitly:

```bash
rsetup-next sources plan cqu
sudo rsetup-next --live-execution sources apply cqu --plan-token PLAN_TOKEN_FROM_PREVIEW --confirm

rsetup-next hardware thermal fan-curve plan --zone thermal_zone0 \
  --device cooling_device0 --point 40:20 --point 55:45 \
  --point 70:75 --point 82:100 --json
sudo rsetup-next --live-execution hardware thermal fan-curve apply \
  --zone thermal_zone0 --device cooling_device0 \
  --point 40:20 --point 55:45 --point 70:75 --point 82:100 \
  --plan-token PLAN_TOKEN_FROM_PREVIEW --confirm
```

Copy `PLAN_TOKEN` from the immediately preceding plan output. A source token
binds the provider and complete source-file contents; a fan-curve token binds
the exact curve request and provider revision, including the persisted
`previous_policy`. If the bound state changes before execution, the command
refuses the stale plan and requires a new preview.

The browser and desktop processes remain unprivileged. The Debian package ships
`/usr/libexec/rsetup-next-helper` and a Polkit policy for live GUI operations.
That helper accepts only fixed catalog action IDs, exact previously reviewed
source, overlay, SPI, or fan-curve plans, validated thermal and LED
configurations, or their fixed boot-time restore verbs. It has no arbitrary
command mode. If authorization is cancelled, the interfaces report
`authorization_failed` without changing the system.

The native fan-curve contract is shared by the CLI commands under
`hardware thermal fan-curve`, HTTP `GET /api/v1/hardware/thermal/fan-curve`,
`POST /api/v1/hardware/thermal/fan-curve/plan`, and
`POST /api/v1/hardware/thermal/fan-curve/apply`, Tauri invokes
`fan_curve_status`, `plan_fan_curve`, and `apply_fan_curve`, and the helper's
fixed `fan-curve-apply REQUEST_JSON PLAN_TOKEN --confirmed` verb. The Web drawer
under Hardware > Thermal uses provider `status.config` and `status.active` for
saved/running/stopped truth; selectors and edited points remain draft-only
until the returned immutable plan is confirmed.

## English and Chinese

The Web and Tauri interfaces detect the browser or operating-system language on
first launch. Use the language control in the top bar to switch between English
and Simplified Chinese; the choice is remembered locally.

CLI and TUI language selection follows `--lang`, then `RSETUP_LANG`, then the
standard `LC_ALL`, `LC_MESSAGES`, and `LANG` environment variables:

```bash
rsetup-next --lang zh-CN status
rsetup-next --lang en actions
RSETUP_LANG=zh-CN rsetup-next --demo tui
```

`auto` is the default. Source-management guidance and results follow the same
locale. JSON output and HTTP API payloads keep stable identifiers
and provider values in their original form, regardless of display language, so
scripts do not change behavior when a user switches locale.

## Desktop application

The Tauri application calls `rsetup-core` directly instead of starting an HTTP
sidecar. The same `ui/` files select the Tauri invoke transport when present and
the HTTP transport in a normal browser.

```bash
cd apps/desktop
npm install
npm run dev
```

Tauri packaging is intentionally outside the root Cargo workspace so normal
CLI/TUI/server development does not download native desktop dependencies.

## Project layout

```text
crates/rsetup-core/       typed telemetry, capability, action and audit models
crates/rsetup-app/        clap CLI, ratatui TUI, axum API and embedded Web assets
ui/                       browser/Tauri control center and presentation locale catalog
apps/desktop/src-tauri/   optional desktop shell
data/pinouts.json         normalized 20-profile SBC pinout catalog
scripts/import-pinouts.mjs reproducible importer for the local pin-out checkout
```

See [the architecture notes](docs/architecture.md) for provider boundaries,
API routes, action execution, and the planned remote-node seam.

## Debian package

The package installs the `rsetup-next` CLI/TUI/Web binary, its narrow privileged
helper, the matching Polkit policy, the thermal, fan-curve, and LED units, and
the `mtd-utils` dependency used by the fixed SPI operations. The fan service
persists its root-only configuration at `/etc/rsetup-next/fan-curve.json` and
runs as `rsetup-next-fan-curve.service`. It does not install
the removed Bash implementation or run the browser process as root. Optional
`device-tree-compiler`, `gpiod`, `v4l-utils`, and `ffmpeg` packages enrich the
corresponding hardware tools.

```bash
make deb-prepare
make deb
```

`deb-prepare` downloads locked Rust dependencies into an ignored, package-local
Cargo cache. The following `dpkg-buildpackage` step runs Cargo in offline mode.
The generated packages are maintained and distributed only through this
repository; they are not intended for submission to the Debian archive.

Packages are built on Debian 13/Trixie with Rust 1.85 or newer, then installed
and smoke-tested on Debian 12/Bookworm. Bookworm is the runtime compatibility
baseline; its Rust 1.63 toolchain is not used for source builds. CI validates
both amd64 and arm64 packages.

SoC vendor marks under `ui/assets` remain the property of their respective
owners. They are used only for device-vendor identification and are not
relicensed under this project's GPL-3+ license.

The normalized 20-profile GPIO catalog is derived from
[`xzl01/pin-out`](https://github.com/xzl01/pin-out). Its copyright owner
authorized the transformed snapshot for distribution under
`GPL-3.0-or-later`; exact source commit and regeneration instructions are in
[`data/PINOUT_PROVENANCE.md`](data/PINOUT_PROVENANCE.md).

## Hardware validation boundary

The Rust workspace, demo provider, CLI, HTTP API, and browser GUI can be tested
on the development host. Live Linux probing still needs validation on supported
Radxa SBCs. The overlay transaction, Overlay-to-Pinout mapping, real camera capture,
sysfs thermal/LED writes, boot-time restore, and backed-up SPI NOR operations
are implemented but have not yet been exercised on physical hardware. In
particular, the temperature-driven curve has not been physically validated
with a real Linux SBC `pwm-fan` cooling device. Overlay filenames and mux modes
still need verification against real packages across the supported Pinout
profiles; unknown generic headers deliberately remain unassigned.
Bootloader, SPI/eMMC, GPIO, overlay, thermal, networking, and power-changing
actions must be tested individually on recoverable hardware before they are
presented as production-ready.

## License

GPL-3.0-or-later, matching the upstream project.

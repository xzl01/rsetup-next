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
| CLI | `rsetup-next status`, `actions`, `run`, `sources`, `doctor` | automation and recovery-friendly output, including JSON |
| TUI | `rsetup-next tui` | keyboard-operated local mission control |
| Web GUI | `rsetup-next serve` | responsive browser control center and JSON API |
| Desktop GUI | `apps/desktop` | Tauri shell using the same static UI and `rsetup-core` |

The core currently probes board identity, operating system, memory, load,
temperature, uptime, storage, network interfaces, selected services, and
hardware capability signals. Its guided operation catalog covers inspection,
system update, SSH enablement, network recovery, root-filesystem expansion,
sleep policy, and reboot.

APT source management is implemented as a guided workflow across the CLI, TUI,
Web, and Tauri surfaces. It detects both traditional `.list` files and Deb822
`.sources` files, limits replacements to known Debian, Ubuntu, and Radxa
endpoints, leaves third-party repositories untouched, and previews every
affected line before confirmation. Live application creates timestamped
backups, writes atomically, runs `apt-get update`, and automatically restores
the previous files if the refresh fails.

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

Synthetic snapshots demonstrate the interface. They are not evidence that a
physical board or peripheral has been tested.

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
rsetup-next doctor
```

On a Linux board, inspect and preview as an ordinary user. Live application is
an explicitly elevated step:

```bash
rsetup-next sources plan cqu
sudo rsetup-next --live-execution sources apply cqu --plan-token PLAN_TOKEN_FROM_PREVIEW --confirm
```

Copy `PLAN_TOKEN` from the immediately preceding plan output. The token binds
application to the provider and complete source-file contents that were
reviewed; if any APT source changes before execution, the command refuses the
stale plan and requires a new preview.

The browser and desktop processes should remain unprivileged. Until the planned
Polkit-authorized helper is packaged, an unprivileged live GUI reports the
stable `root_required` error instead of attempting to elevate the whole UI.

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
```

See [the architecture notes](docs/architecture.md) for provider boundaries,
API routes, action execution, and the planned remote-node seam.

## Debian package

The package installs the `rsetup-next` CLI/TUI/Web binary. It does not install
the removed Bash implementation or run the browser process as root.

```bash
make deb-prepare
make deb
```

`deb-prepare` downloads locked Rust dependencies into an ignored, package-local
Cargo cache. The following `dpkg-buildpackage` step runs Cargo in offline mode.
For Debian archive submission, replace this cache with Debian-packaged or
properly vendored crates as described in `debian/README.source`.

Packages are built on Debian 13/Trixie with Rust 1.85 or newer, then installed
and smoke-tested on Debian 12/Bookworm. Bookworm is the runtime compatibility
baseline; its Rust 1.63 toolchain is not used for source builds. CI validates
both amd64 and arm64 packages.

## Hardware validation boundary

The Rust workspace, demo provider, CLI, HTTP API, and browser GUI can be tested
on the development host. Live Linux probing still needs validation on supported
Radxa boards. Bootloader, SPI/eMMC, GPIO, overlay, thermal, networking, and
power-changing actions must be tested individually on recoverable hardware
before they are presented as production-ready.

## License

GPL-3.0-or-later, matching the upstream project.

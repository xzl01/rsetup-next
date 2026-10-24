# rsetup next

[![CI](https://github.com/xzl01/rsetup-next/actions/workflows/ci.yml/badge.svg)](https://github.com/xzl01/rsetup-next/actions/workflows/ci.yml)

One control plane for a Linux SBC, available as a scriptable CLI, an interactive
TUI, a loopback Web console, and an optional Tauri desktop application.

This is the Rust-based successor to the original
[`radxa-pkg/rsetup`](https://github.com/radxa-pkg/rsetup). The new Rust control
plane owns probing, policy, and action execution directly; it does not invoke or
require the legacy `rsetup` command at runtime.

## What is working

| Surface | Entry | Purpose |
| --- | --- | --- |
| CLI | `rsetup-next status`, `actions`, `run`, `sources`, `hardware`, `system`, `doctor` | automation and recovery-friendly output, including JSON |
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

Local system, SoC, firmware, thermal and block-device inspection uses the GPL-3.0-or-later
[`deviceinfo` library](https://github.com/shallow-out/devicesinfo/tree/716ad13ca316e0d6c0b36b87df6a3107ace31c45),
pinned to that commit as the `externals/devicesinfo` Git submodule and linked
through a Cargo path dependency. No `deviceinfo` executable or new
runtime package is needed. CPU utilization is a guarded difference between
system samples, not load average; a fresh probe takes one bounded 200 ms
baseline window so `status`, TUI startup and the first Web poll report a real
value. Missing metrics remain `null` in JSON and unavailable in CLI/TUI/Web.
Mutations, Polkit and the existing NVMe/MMC health readers remain owned by Next.

APT source management is implemented as a guided workflow across the CLI, TUI,
Web, and Tauri surfaces. It detects both traditional `.list` files and Deb822
`.sources` files, limits replacements to known Debian, Ubuntu, and Radxa
endpoints, leaves third-party repositories untouched, and previews every
affected line before confirmation. Live application creates timestamped
backups, writes atomically, runs `apt-get update`, and automatically restores
the previous files if the refresh fails.

Mirror speed testing is read-only and needs no administrator authorization.
Use **Test mirrors** in the package-mirror panel, `b` on a selected mirror in
the TUI, or `rsetup-next sources benchmark --mirror official --json` (omit
`--mirror` to test the catalog). Each provider samples one configured system
`InRelease` index and, where applicable, one Radxa index. Results separate
time-to-first-byte from sample transfer rate; small indexes do not measure peak
bandwidth or prove that every package/pocket is available. At most 512 KiB is
consumed per index, with a 2-second connection and 6-second total timeout.
Only catalog HTTPS endpoints are used, TLS is verified, redirects are not
followed, and curl configuration files are ignored. No APT refresh, file write,
or automatic mirror selection occurs. Demo mode uses labeled synthetic results.
Selecting a tested mirror still requires preview and confirmation before apply.

Writing a system image to a board's UFS, eMMC, SD card, or NVMe is available as
`rsetup-next system targets|plan|install`. `targets` and `plan` are read-only:
they list the candidate devices and every partition a write would destroy, the
image format, the uncompressed write size, and the SHA-256 binding of the first
1 MiB. `install` stays a preview unless `--live-execution` is given on Linux, and
it applies only after the exact target device name is typed; the device that
carries the running root or boot filesystem is refused permanently, and a
mounted target is refused with its mount points. A write cannot be rolled back,
and a failing write reports how many bytes are already on the device.
`.img.xz` images are previewed by a Rust parser and decompressed through
`xz -dc` (the `xz-utils` dependency) only while they are written. The same
procedure is available from the CLI, the TUI panel (press `i`), the loopback
HTTP API (`GET/POST /api/v1/storage/install/…`, where an image is an id from a
fixed server-side catalogue and mutating calls carry `x-rsetup-request: 1`), the
Tauri desktop shell, and the browser's **Install** page. Each surface refuses the
write unless live execution is enabled, so a demo or preview session cannot
write. The full procedure, the stable failure codes, and the on-board
verification checklist for the Radxa Cubie A7Z are in
[docs/system-install.md](docs/system-install.md).

The native hardware manager now covers seven migrated workflows:

- Device-tree overlays are listed from the managed boot directory, checked
  for declared resource conflicts and package requirements, previewed with an
  exact revision-bound token, and renamed transactionally before
  `u-boot-update` on U-Boot systems. UEFI + DT uses a separate native EDK2/BLS
  backend shared by Q6A and Q8B, following the layout in upstream rsetup's
  [edk2-menu.sh](https://github.com/radxa-pkg/rsetup/blob/main/src/usr/lib/rsetup/cli/edk2-menu.sh)
  without invoking it. The running kernel's exact Type #1 entry and ordered
  `devicetree-overlay` references determine saved selection, not file suffixes.
  The preview names the kernel, entry and DTB/DTBO paths. It does **not** change
  the default entry or other kernel versions: changes activate only when booting
  that kernel. `fdtoverlay` validates the selection before private backups,
  atomic BLS replacement and failure rollback. Ambiguous/unsafe layouts fail
  closed; EFI never falls back to leftover U-Boot configuration.
  Protected ESP files require an explicit **Authorize read** action (CLI:
  `hardware overlays status --authorize --json`, or `plan` / `gpio` with
  `--authorize`). This fixed helper operation is read-only. Authorized snapshots
  stay in process memory, are labeled as cached, and can be re-read explicitly;
  applying always revalidates current root-owned inputs against the plan token.
- The 40-pin GPIO header is a read-only map backed by 20 normalized SBC
  profiles from `xzl01/pin-out` plus an official Radxa Dragon Q8B profile.
  Each physical pin shows exactly one configured
  function: the saved enabled Overlay assignment takes priority, otherwise a
  known SBC shows its exact default function (`Function1` in pin-out,
  `Function0` in Q8B's documentation). Unknown
  generic headers remain unassigned.
  The drawer omits GPIO-chip, line, direction, consumer, and kernel-ownership
  metadata. Saved Overlay changes are shown immediately and marked as requiring
  a reboot to activate; the status path never invokes `gpioget` or requests a
  GPIO line.
  EFI/BLS saved selections feed the same GPIO resolver. When configuration
  cannot be read, only official defaults are shown, not a claim of active mux
  state. Real overlay activation and electrical behavior still require hardware
  testing after a separately confirmed write and reboot.
- Verified Video4Linux capture nodes can capture a bounded single-frame webcam
  test through `ffmpeg`. Discovery uses read-only `VIDIOC_QUERYCAP`, excluding
  codec/M2M, output, metadata and unverified nodes. Device IDs are enumerated
  and validated rather than accepted as arbitrary paths.
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
- Writing a system image is destructive and irreversible: it needs
  `--live-execution` on Linux, an exact typed device name, and it never selects
  the device that carries the running root or boot filesystem. A failed write
  reports the bytes already on the device; there is no rollback.
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

Clone with submodules included:

```bash
git clone --recurse-submodules https://github.com/xzl01/rsetup-next.git
cd rsetup-next
```

For an existing checkout, run `git submodule update --init --recursive` before
building. GitHub's automatic source ZIP/tarballs omit submodule contents; use a
recursive clone for source builds. The submodule revision belongs to the parent
repository, so updating Next also requires updating its submodules.

```bash
# The frontend is generated on demand: the first target that compiles
# rsetup-controller runs `npm ci` and `vite build` for you, because
# apps/controller-web/dist is a build product and is not committed.
make build
make test

# Inspect the current host. macOS automatically returns demo telemetry.
cargo run -p rsetup-next -- status

# Explicit demonstration mode.
cargo run -p rsetup-next -- --demo tui
cargo run -p rsetup-next -- --demo serve
```

Open `http://127.0.0.1:8788` after starting the server.

`build` and `test` — like `lint`, `metrics-gates` and
`controller-test-offline`, which also compile the workspace — depend on
`web-dist-check`, the gate that keeps `apps/controller-web/dist/index.html` in place
before cargo runs: when the file is missing the target runs `make web-build` for you,
and when it is already there it only confirms it, so a warm tree never pays for a
second npm install. `apps/controller-web/dist` is a **generated artifact** — the whole
directory is git-ignored — so the frontend is compiled locally, in CI and in the Debian
package build rather than shipped as a committed binary blob, and no file name or hash
of a generated asset is pinned anywhere in the tree.

The toolchain is the current stable Rust (verified locally with rustc 1.99.1). No
minimum supported Rust version is pinned any more: the manifests carry no
`rust-version`, and there is no separate lower-version job in CI or in the Makefile.

After changing anything under `apps/controller-web` there is nothing extra to commit:
`make web-build` regenerates `dist`, and `web-dist-check` calls it for you the next time
you run a compiling target. `make web-build` is the explicit rebuild entry point, and it
works through **two** stamp files:

| Stamp | Rebuilt when | Recipe |
| --- | --- | --- |
| `apps/controller-web/node_modules/.rsetup-npm-ci-stamp` | `package.json` or `package-lock.json` changes | `npm --prefix apps/controller-web ci $(NPM_CI_FLAGS)` |
| `apps/controller-web/dist/.rsetup-build-stamp` | `index.html`, `vite.config.ts`, `tsconfig*.json`, a non-test file under `apps/controller-web/src/` — the directory itself is a prerequisite too, so added and deleted files count — or Vite's `public/` directory changes | `npm --prefix apps/controller-web run build` |

The split is the point: editing a component re-runs only `vite build`, never `npm ci`.
`*.test.ts` and `*.test.js` are not `vite build` inputs, so touching a spec does not
rebuild `dist` either. Both stamp files live under git-ignored paths
(`node_modules/` and `dist/`), which is also why `dist` itself never shows up in a diff.

Building the frontend is the one step that needs Node 22, npm and — on a first run with
a cold cache — network access. Two ways to keep it offline:

```bash
# 1. npm cache already warm on this machine: the flag goes straight to `npm ci`.
make web-build NPM_CI_FLAGS=--offline

# 2. node_modules/ prepared some other way: write the stamps yourself so make stops
#    trying to rebuild. make only compares stamp timestamps, never the directories.
touch apps/controller-web/node_modules/.rsetup-npm-ci-stamp
touch apps/controller-web/dist/.rsetup-build-stamp
```

Copying `node_modules` in without the stamp files does not help: make still sees the
prerequisites as newer and runs `npm ci` again, which is exactly what fails offline.
Either route also satisfies `web-dist-check`, which is what lets a network-isolated
machine keep compiling: it builds the frontend only when `dist` is genuinely missing.

`make metrics-static-gates` runs only the read-only Prometheus metric-line grep
gates: no compilation, no network access and no frontend build, so CI can call it on
its own. One of those gates rejects committed configuration or credential files, and it
checks two lists — the staged index (`git diff --cached --name-only`) and the tracked
file list (`git ls-files`) — because a CI checkout has an empty index, where a
staged-only check would pass without ever looking at anything. `make metrics-gates`
additionally runs `fmt-check`, the frontend check (`web-dist-check`, which builds `dist`
when it is missing), clippy and the controller test
suites.

For Rust format checks run `make fmt-check` rather than `cargo fmt --all`,
because the latter also pulls the third-party `externals/devicesinfo` submodule
into the checked scope; see the "Rust 格式检查口径" section of
[AGENTS.md](AGENTS.md).

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
configurations, their fixed boot-time restore verbs, or the read-only
`overlays-inspect` verb (no path or command arguments). It has no arbitrary
command mode. If authorization is cancelled, the interfaces report
`authorization_canceled` without changing the system.

The desktop launcher defaults to inspection/preview. Start
`rsetup-desktop --live-execution` (or `npm run dev -- -- --live-execution`
from `apps/desktop`) to opt into live operations; each change still requires
its confirmation and Polkit authorization. `--demo` never mutates hardware.
The desktop build stages production UI files only; browser tests are not bundled.

The HTTP server rejects non-loopback listeners and non-local Host/Origin headers.
For remote use, keep the server on the SBC loopback and forward it with
`ssh -L 8788:127.0.0.1:8788 radxa@SBC_ADDRESS`, then open
`http://127.0.0.1:8788`. Non-browser API clients must include
`X-Rsetup-Request: 1` on POST requests. This is a browser-origin boundary,
not authentication against other local processes. Do not expose it via a public proxy.

The native fan-curve contract is shared by the CLI commands under
`hardware thermal fan-curve`, HTTP `GET /api/v1/hardware/thermal/fan-curve`,
`POST /api/v1/hardware/thermal/fan-curve/plan`, and
`POST /api/v1/hardware/thermal/fan-curve/apply`, Tauri invokes
`fan_curve_status`, `plan_fan_curve`, and `apply_fan_curve`, and the helper's
fixed `fan-curve-apply REQUEST_JSON PLAN_TOKEN --confirmed` verb. The Web drawer
under Hardware > Thermal uses provider `status.config` and `status.active` for
saved/running/stopped truth; selectors and edited points remain draft-only
until the returned immutable plan is confirmed.

Fan curves require a thermal zone bound exclusively to the selected PWM fan,
with no other zone controlling that fan. Mixed CPU/GPU cooling zones are rejected
to preserve kernel throttling. Bindings are checked again on every daemon tick;
invalid saved bindings trigger recovery of the kernel governor.

SPI apply operations share a cross-process lock across all MTD targets, held
through backup, erase, write, verification, and any rollback.

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

## Controller

`rsetup-controller` is a standalone HTTP control-plane binary, separate from the
`rsetup-next` CLI/TUI/server and packaged on its own as `rsetup-controller` (built
from this same source package; see "Install from the Debian package" below). Build it as
part of the workspace — `web-dist-check`
builds the frontend `dist` it embeds at compile time when that directory is missing, so
there is no separate build step to remember (see "Build and run") — and start it with:

```bash
cargo run -p rsetup-controller
```

Required environment variables; the process exits instead of starting with any of
them missing or empty:

| Variable | Purpose |
| --- | --- |
| `CONTROLLER_DATABASE_URL` | MySQL/TiDB connection URL holding the controller schema. |
| `CONTROLLER_ALLOWED_HOSTS` | Exact comma-separated `Host` allow-list. |
| `CONTROLLER_ALLOWED_ORIGIN` | The single exact browser `Origin`. |
| `CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX` | Hex-encoded management cursor secret. |

Optional variables: `CONTROLLER_LISTEN_ADDRESS` (defaults to `127.0.0.1:8080`),
`CONTROLLER_TRUST_TLS` (only the exact values `1` and `true` append `Secure` to the
session cookie, for deployments whose reverse proxy terminates TLS; unset or empty
leaves it off, any other value is a startup error),
`CONTROLLER_BOOTSTRAP_SECRET_LOG` (path of the file that receives the one-time
administrator credential, default `controller-bootstrap-secret.log` relative to the
working directory), and `NTP_SERVERS` (comma- or whitespace-separated NTP sources for
the startup time gate; without them the gate falls back to the system clock and the
task line keeps reporting untrusted time evidence).

The metric line reads three more: `CONTROLLER_CONFIG` (configuration file path, default
`controller.toml`, which holds the scrape credentials and must stay out of the
repository — the static gate rejects a tracked `controller.toml` or `secret/` entry),
`PROMETHEUS_AUTH_USER` and `PROMETHEUS_AUTH_PASSWORD` (override of the Basic-auth
identity, accepted only as a pair; setting one without the other is a startup error).
The common variables are listed here; the full configuration contract is in the
[metrics endpoint design document](docs/superpowers/specs/2026-10-19-prometheus-metrics-endpoint-design.md).

The default bind address is loopback only, and the HTTP surface assumes a controlled,
trusted management network: it is not designed for exposure to untrusted networks or
for direct Internet access, and it does not terminate TLS itself. Nothing is
pre-provisioned: on the first start against an uninitialized schema the process
generates a one-time administrator credential and writes it to the private file above
(created with `0600` and `O_EXCL`; the parent directory is not protected, so start the
process from a private directory). Read that credential once, log in, change it, and
delete the file.

`rsetup-admin` is the local emergency path for that administrator
account. It takes no arguments, issues a fresh one-time credential to the controlling
terminal, and records
an audit event. Its gate is the OS real and effective UID plus a real TTY on stdin,
stdout and stderr and a writable `/dev/tty`. Non-interactive SSH command execution,
cron and a pipe therefore cannot drive it; an interactive `ssh -t` session still works,
because it allocates a pty and the same root-and-TTY checks then pass as they would at
a local keyboard. The gate is about terminal attachment and privileges, not about
physical locality.

### Install from the Debian package

The `rsetup-controller` binary package comes out of this same source package. Its build
step generates the web UI with npm (`npm --prefix apps/controller-web ci` and
`npm --prefix apps/controller-web run build`, both in `debian/rules`) before the first
cargo command, because `build.rs` embeds `apps/controller-web/dist` at compile time and
that directory is not part of the source package; only the Rust side stays offline. It
installs

| Path | Purpose |
| --- | --- |
| `/usr/bin/rsetup-controller` | the control-plane process (unit `rsetup-controller.service`) |
| `/usr/bin/rsetup-controller-migrate` | explicit schema migration, `--check` / `--apply` |
| `/usr/bin/rsetup-admin` | local root emergency admin recovery (cargo bin name equals the delivered name; source `src/bin/reset-admin.rs`) |
| `/usr/share/doc/rsetup-controller/rsetup-controller.default` | `EnvironmentFile` example, 0644, secret-free — **not** installed to `/etc`; the operator creates `/etc/default/rsetup-controller` from it |
| `/usr/lib/systemd/system/rsetup-controller.service` | unit: `User=rsetup-controller` (static system account), `StateDirectory=rsetup-controller`, `StateDirectoryMode=0700` |
| `/usr/share/doc/rsetup-controller/README.Debian` | the full post-install walkthrough |

The unit drops privileges to the static system account `rsetup-controller` (`User=` and
`Group=`). The package creates that account and its private group on install with
`adduser --system --group --home /var/lib/rsetup-controller --no-create-home
--disabled-login`, idempotently, and the uid/gid never changes afterwards:
`/var/lib/rsetup-controller` keeps one stable owner (0700,
`rsetup-controller:rsetup-controller`) across restarts and upgrades, so backup, rsync and
ACL policies have a stable anchor. `DynamicUser=yes` is deliberately gone: a dynamic user
gets a fresh temporary uid on every start, systemd re-chowns the state directory each time,
and the owner of persistent state drifts across restarts, which leaves forensics and
authorization policy without an anchor. The maintainer script only creates the account; it
never starts or enables a service, and the account is kept on purge (no `deluser`) — see
`debian/rsetup-controller.README.Debian`.

The package enables and starts nothing.
`dh_installsystemd --no-enable --no-start --no-stop-on-upgrade` keeps the unit out of the
boot path and out of every start/restart snippet of the generated maintainer scripts —
`--no-enable` alone would still leave the `deb-systemd-invoke` activation snippet (a start,
or on upgrade a `_dh_action=restart`) in the postinst — and the
unit also refuses to run while its `EnvironmentFile` is missing, so an unconfigured install
is simply inactive. Completing the setup is an
operator action, and nothing below has been run against a real database on your behalf:

1. Create the configuration file with its final permissions, then fill it in:
   `sudo install -m0600 -o root -g root /usr/share/doc/rsetup-controller/rsetup-controller.default /etc/default/rsetup-controller`
   followed by `sudoedit /etc/default/rsetup-controller` with `CONTROLLER_DATABASE_URL`,
   `CONTROLLER_ALLOWED_HOSTS`, `CONTROLLER_ALLOWED_ORIGIN`,
   `CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX`. The packaged example is 0644 precisely
   because it holds no secrets; `install -m0600` (rather than `cp` then `chmod`) is what
   keeps the future `EnvironmentFile` from ever existing world-readable while it is being
   filled with a DSN.
2. Take a real, restorable backup of the target database, then export the four
   migration gates next to `CONTROLLER_DATABASE_URL`:
   `CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE=1`,
   `CONTROLLER_MIGRATION_EXPECTED_DATABASE` (the real connected database name),
   `CONTROLLER_MIGRATION_BACKUP_REF` (a reference to that backup) and
   `CONTROLLER_MIGRATION_ACK=isolated-exclusive-backed-up-disposable`. They are
   attestation, not a guess the tool may fill in: the migration fails closed if the
   declared database is not the one the server reports.
3. `rsetup-controller-migrate --check` (read-only, prints only
   `schema_version=<number>` or `schema_version=missing`), then
   `rsetup-controller-migrate --apply`. Schema v4 is the only baseline: an empty database
   is created with the full v4 schema, a database already at v4 only gets the read-only
   consistency pass (idempotent). A v1, v2, v3 or unknown database is refused before the
   first write — there is no upgrade path and no legacy fixture mode, so a pre-v4 database
   must first be reset at table level by the operator (the real, restorable backup in step
   2 is what makes that safe). It needs no root — privileges come from the database account
   — and never reads `/dev/tty`.
4. `systemctl start rsetup-controller` (add `enable` once you want it at boot).
5. On first boot the one-time administrator credential lands in
   `/var/lib/rsetup-controller/bootstrap-secret.log` (`0600`, `O_EXCL`); read it, log in,
   change the password, delete the file.
6. `rsetup-admin` recovers a locked-out administrator account from a
   local root interactive terminal.
7. The listener stays on `127.0.0.1:8080` by default; reaching it beyond this host means
   a reverse proxy with TLS and a trusted management network, plus matching
   `CONTROLLER_ALLOWED_HOSTS`/`CONTROLLER_ALLOWED_ORIGIN` values.

Not wired yet: the transport line (device live cache, recovery controller, admission
notifications, board task client) is assembled as fail-closed placeholders that report
"not wired" or device-offline instead of invented values, the SSE event bus has no
business publisher beyond heartbeats and its own resets, and the device snapshot store
has no writer yet. The endpoints behind those unwired sources answer 503 with
`STORAGE_UNAVAILABLE` — the placeholder surfaces as a configuration error and its
message is never echoed — or return a well-formed "no sample" body (`snapshot: null`,
`freshness: "unknown"`, `age_ms: null`) rather than a fabricated reading. `NOT_READY`
is the code for schema and crypto unreadiness, and the transport placeholders do not
report it. See `docs/testing/controller-v1-known-gaps.md` for the full list before
operating anything with real boards attached.

### Board-agent local test harness

`crates/rsetup-board-agent` now has an amd64-runnable test binary. It exercises the real
typed dispatch path (capabilities/status/clock over `/proc`, journal lookup, error
mapping) and a loopback JSON-lines console; it does **not** provide the production
encrypted tunnel and never performs a real host reboot.

```bash
cargo build -p rsetup-board-agent --release --bin rsetup-board-agent
./target/release/rsetup-board-agent --once --journal-dir /tmp/rsetup-board-agent-journal
./target/release/rsetup-board-agent --watch --interval-secs 5 --journal-dir /tmp/rsetup-board-agent-journal
./target/release/rsetup-board-agent --loopback-test-server --listen 127.0.0.1:39876 \
  --journal-dir /tmp/rsetup-board-agent-journal
```

See [crates/rsetup-board-agent/README.md](crates/rsetup-board-agent/README.md) for the
JSON-lines protocol and its scope limits.

Board-side delivery and running user: `rsetup-board-agent` is its own Debian binary package
built from this source package, installing `/usr/bin/rsetup-board-agent` and the unit
`/usr/lib/systemd/system/rsetup-board-agent.service` (`dh_installsystemd --no-enable
--no-start --no-stop-on-upgrade`, so disabled by default and never started by the
package). The unit runs the read-only sampling watch loop as
`User=root`/`Group=root` (`--interval-secs 10 --journal-dir /var/lib/rsetup-board-agent`,
with the state directory created 0700 by `StateDirectory`): the board's hardware and system
state sources are not uniformly readable by an unprivileged user, so both the production
and the test deployment of the board side run as root, inside a sandbox of
`ProtectSystem=strict`, `NoNewPrivileges=yes`, `PrivateTmp=yes`, `ProtectHome=yes` and
`LockPersonality=yes` with a single `ReadWritePaths`. A manual run in a shell elevates
nothing and stays with the invoking user. **The delivery boundary is unchanged**: the
production encrypted tunnel is not wired up, so the unit starts a sampling loop that only
writes to the local journald and opens no encrypted session with a controller;
`device.reboot.prepare` and `device.reboot.execute` answer capability unavailable, and the
real reboot boundary remains unimplemented.

Supported and CI-verified architectures are `arm64` (aarch64) and `amd64` (x86_64);
32-bit targets are no longer maintained.

## Project layout

```text
crates/rsetup-core/       typed telemetry, capability, action and audit models
crates/rsetup-app/        clap CLI, ratatui TUI, axum API and embedded Web assets
ui/                       browser/Tauri control center and presentation locale catalog
apps/desktop/src-tauri/   optional desktop shell
externals/devicesinfo/    pinned upstream Git submodule (read-only probe library)
data/pinouts.json         normalized 20-profile SBC pinout catalog
data/pinouts/dragon-q8b.json  official Q8B profile (maintained separately)
scripts/import-pinouts.mjs reproducible importer for the local pin-out checkout
```

See [the architecture notes](docs/architecture.md) for provider boundaries,
API routes, action execution, and the planned remote-node seam.

## Debian package

The package installs the `rsetup-next` CLI/TUI/Web binary, its narrow privileged
helper, the matching Polkit policy, the thermal, fan-curve, and LED units, and
the `mtd-utils` dependency used by the fixed SPI operations. The fan service
persists its root-writable, user-readable configuration at `/etc/rsetup-next/fan-curve.json` and
runs as `rsetup-next-fan-curve.service`. It does not install
the removed Bash implementation or run the browser process as root. Optional
`device-tree-compiler`, `gpiod`, `v4l-utils`, and `ffmpeg` packages enrich the
corresponding hardware tools.

The same source package also builds two more packages. `rsetup-controller`: the
control-centre process, its two operator CLIs (`rsetup-controller-migrate` and
`rsetup-admin`), one systemd unit and a secret-free configuration example under
`/usr/share/doc/rsetup-controller/` (nothing is written to `/etc/default` — creating that
file is the operator's step); the
unit drops privileges to the static system account `rsetup-controller` created on install.
What it installs, and the migration and configuration steps an operator still has to
perform, are in "Install from the Debian package" under "Controller". `rsetup-board-agent`:
the board-side agent binary and its own systemd unit, running as root in the read-only
watch loop; the production encrypted tunnel and a real reboot boundary are not wired up.
Building any of them needs Node.js and npm, both Build-Depends: `debian/rules` generates
the controller's web UI with `npm ci` + `npm run build` before cargo runs, while the Rust
side keeps `CARGO_NET_OFFLINE` and the package-local Cargo cache.
Both systemd units are installed disabled and are never started by the package
(`dh_installsystemd --no-enable --no-start --no-stop-on-upgrade`), and the
controller unit additionally refuses to run while its configuration file is missing, so
installing the packages starts nothing
and touches no database.

The package depends on `xz-utils` because writing a compressed `.img.xz` system
image decompresses it with `xz -dc`. The choice of a hard dependency over
`Recommends`, and the fallback when the tool is absent, are recorded in
[`debian/README.source`](debian/README.source). The system-image guide ships as
`/usr/share/doc/rsetup-next/system-install.md`.

```bash
make deb-prepare
make deb
```

Initialize the Git submodules before preparing the package. `deb-prepare`
downloads locked Rust dependencies into an ignored, package-local
Cargo cache. The following `dpkg-buildpackage` step runs Cargo in offline mode.
The generated packages are maintained and distributed only through this
repository; they are not intended for submission to the Debian archive.

Packages are built on Debian 13/Trixie with its current stable Rust toolchain, then
installed and smoke-tested on Debian 12/Bookworm. Bookworm is the runtime compatibility
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
The additional Q8B profile is derived from Radxa's GPIO documentation and
retains its CC-BY-4.0 attribution and license separately.

## Hardware validation boundary

The Rust workspace, demo provider, CLI, HTTP API, and browser GUI can be tested
on the development host. Q8B live read-only probing has been checked on Ubuntu
26.04.1 with UEFI + DT; see the [test report](docs/testing/q8b-2026-09-07/report.md)
for results and limitations. Other supported SBCs still need live validation.
Writing a system image to real UFS/eMMC/SD/NVMe storage is implemented and
covered by injected-fixture and dry-run tests, but no write has been executed on
a physical board yet. The command sequence, expected output, failure criteria,
and the split between what is verified locally and what is still pending are in
[docs/system-install.md](docs/system-install.md); host-side evidence is in the
[local validation report](docs/testing/system-install-2026-10-13/report.md).
Device-level checks cannot run in CI and are not CI gates.
The overlay transaction, Overlay-to-Pinout mapping, real camera capture,
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

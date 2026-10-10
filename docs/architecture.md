# rsetup next architecture

## One control plane, four surfaces

`rsetup-core` owns all facts that must agree across interfaces: the device
snapshot, capability detection, action metadata, execution policy, action
results, and activity record. CLI, TUI, HTTP, and Tauri translate user intent
into this domain model; they do not implement board operations independently.

```text
CLI ─────┐
TUI ─────┼── Controller ── Probe provider ── /proc, /sys, systemd
HTTP GUI ┤       │
Tauri ───┘       ├──────── Fixed action catalog ── built-in Rust executors
                 ├──────── APT source manager ──── validated mirror catalog
                 ├──────── Hardware manager ────── overlays, GPIO, V4L2, thermal
                 └──────── Polkit helper ───────── root-only fixed protocol
```

The current `Controller` is in-process. Its serializable models and stable
action identifiers are deliberately transport-neutral so a future authenticated
remote-node client can implement the same interface without changing the user
surfaces.

Presentation localization stays outside `rsetup-core`. The Rust application
localizes human-readable CLI/TUI output, while `ui/i18n.js` localizes the shared
browser/Tauri surface. Both catalogs key known actions and capabilities by their
stable identifiers and fall back to provider copy for extensions. JSON output
and HTTP payloads remain locale-neutral.

## Observation providers

`ProbeMode::Auto` chooses live Linux inspection on Linux and the demo provider
elsewhere. `RSETUP_MODE=demo` and `--demo` force the synthetic provider;
`RSETUP_MODE=live` forces the Linux provider.

The Linux provider uses the GPL-3.0-or-later `deviceinfo` v0.2.0 library,
pinned to `0d6c947f555344b072000694c431e1593a47a72c` by the
`externals/devicesinfo` Git submodule. Both the root workspace and desktop build
use its `crates/deviceinfo` Cargo path dependency. The upstream workspace is
excluded from Next's workspace: its CLI is neither built nor installed as part
of Next. CI checks out submodules recursively; offline Debian builds require
the initialized source checkout plus the prepared registry cache.
Next's formatting checks explicitly select its two workspace packages because
`cargo fmt --all` also follows local path dependencies into upstream sources.
`deviceinfo_probe` is the adapter, not an external executable:

- `inspect_system` and `inspect_platform` provide CPU architecture, SBC/SoC,
  OS and firmware identity. UEFI, DT and ACPI are independent observations;
  UEFI + DT does not disable Next's EFI Overlay backend.
- Identity is cached for 30 seconds. Boot ID, namespace, hostname or kernel
  changes invalidate it immediately. Each `Controller` owns its cache;
  controller clones share it, independent controllers do not.
- `observe_system` and `observe_thermal` supply changing memory, load, uptime,
  trusted temperatures and filesystem usage. `inspect_storage` refreshes block
  inventory on every snapshot so hotplug remains visible. Mounts associate by
  major:minor; `/boot` on the root filesystem is not counted twice.
- CPU utilization uses `Snapshot::cpu_usage_since` with the complete previous
  envelope. Resets, reboot, namespace changes, missing counters and
  overlapping/frozen sampling windows are unknown, never fabricated zeroes.
  Collection is serialized per controller. A fresh probe has no previous
  envelope, so its first observation takes one bounded in-process baseline
  window (200 ms) and keeps the earlier sample as `previous`; that is what lets
  a one-shot consumer (`status`, TUI startup, the first HTTP poll) report a real
  utilization instead of waiting for its next observation. Later observations
  compare against the previous envelope and never sleep.
- Network and service observation still uses Next's `/sys/class/net`, `ip`
  and selected `systemctl is-active` readers. Thermal policies and every write
  remain in Next. The thermal drawer uses deviceinfo temperatures/cooling-state
  validation while policy discovery, write validation and persistence stay local.

The existing JSON field names are preserved, but missing metrics (including
`cpuPercent`, `loadAverage`, memory byte counts and `uptimeSeconds`) are nullable.
Live snapshots add `probe`: provider/schema, separate dynamic and cached source
contexts, independent firmware observations, diagnostics and warnings. Upstream
context/diagnostic/platform objects preserve deviceinfo's snake_case schema;
nanosecond timestamps remain decimal strings to avoid JavaScript precision loss.
Demo snapshots omit this evidence and never call deviceinfo. Free-text warnings
are informational, not inputs to control decisions.

This integration calls only local read-only inspection/observation APIs. It does
not use `check_environment`, SSH capture, runtime scanning or storage-health
ioctls. Captured test roots never inherit the collector's clocks/filesystem
usage. NVMe/MMC health ioctl readers, authorization and their error mapping
remain unchanged. Normal polling cannot start a command through deviceinfo,
drive GPIO or trigger firmware/health operations. Debian packages compile the
library into the binary; they have no deviceinfo runtime dependency.

A failed optional probe produces an unavailable or unknown signal instead of
turning the entire device snapshot into a failure.

## Guided action execution

Each `ActionSpec` declares an identifier, category, description, risk level,
root requirement, estimate, visible procedure steps, and a fixed command. The
command is excluded from API serialization.

Execution has two independent gates:

1. `ExecutionPolicy::DryRun` is the default and produces a synthetic result.
2. Guarded or more severe actions require explicit confirmation.

For a root-required live operation, an already-root CLI executes directly.
Otherwise the controller asks Polkit to start `/usr/libexec/rsetup-next-helper`.
The browser, HTTP server, TUI, and desktop shell remain unprivileged. The helper
accepts only fixed protocol verbs: a catalog action, an exact source plan, an
exact overlay plan, a validated thermal policy, or boot-time thermal policy
restoration. It returns a typed JSON result and does not accept executable
paths, arbitrary file paths, command arguments, or shell text. Cancellation and
failed authorization are returned as a stable `authorization_failed` error.

The live action catalog is rebuilt when requested and checks package, systemd
unit, and command prerequisites. Unsupported operations remain discoverable but
are marked unavailable with a reason in each surface.

The `system.change-sources` workflow is parameterized and therefore cannot be
executed through the generic action endpoint. Its dedicated controller methods
accept a mirror provider ID from a fixed catalog. Planning is unprivileged;
live application requires root, creates a same-directory backup for every
affected file, uses atomic replacement, and runs `apt-get update`. A failed
metadata refresh restores all files from those backups. URL matching is limited
to recognized Debian, Ubuntu, and Radxa hosts; unknown third-party repositories
are never rewritten. Every plan includes a deterministic token over the selected
provider, source revision, and complete transformed documents. Apply rebuilds
the plan from disk and rejects missing or stale tokens before writing.

## Native hardware manager

`rsetup-core::hardware` owns hardware-specific observation and mutation.
Overlay IDs must be basenames ending in `.dtbo`; video IDs must match an
enumerated `videoN`; thermal policies must be present in the kernel's common
policy set. These identifiers are validated again inside the root helper.

Overlay planning fingerprints the complete enabled state and selected IDs.
Applying a stale token is rejected. File renames are rolled back if any rename
or `u-boot-update` fails. GPIO inspection is intentionally read-only. Camera
capture is time-bounded and size-bounded. Thermal policy writes preserve the
upstream `pwm-fan` incompatibility guard and store only one validated policy
under `/etc/rsetup-next`; a packaged oneshot unit restores it at boot.

## HTTP API

The loopback server exposes:

| Method | Route | Meaning |
| --- | --- | --- |
| `GET` | `/api/v1/health` | process liveness |
| `GET` | `/api/v1/snapshot` | current device state |
| `GET` | `/api/v1/actions` | guided operation catalog |
| `POST` | `/api/v1/actions/{id}/run` | execute or dry-run one catalog action |
| `GET` | `/api/v1/sources` | detected APT source state and trusted mirror catalog |
| `POST` | `/api/v1/sources/benchmark` | read-only bounded index samples for `{ "providerId": "official" }`; no root or APT update |
| `POST` | `/api/v1/sources/plan` | preview managed source changes for one provider ID |
| `POST` | `/api/v1/sources/apply` | confirm and apply an exact plan token; stale plans are rejected |
| `GET` | `/api/v1/hardware/overlays` | list managed overlay state |
| `POST` | `/api/v1/hardware/overlays/authorize` | explicitly authorize a read of protected EFI configuration; no writes |
| `POST` | `/api/v1/hardware/overlays/plan` | validate and preview an overlay selection |
| `POST` | `/api/v1/hardware/overlays/apply` | apply an exact overlay plan |
| `GET` | `/api/v1/hardware/gpio` | read the 40-pin GPIO map |
| `GET` | `/api/v1/hardware/video` | list Video4Linux capture devices |
| `POST` | `/api/v1/hardware/video/capture` | capture one bounded test frame |
| `GET` | `/api/v1/hardware/thermal` | inspect thermal zones and cooling devices |
| `POST` | `/api/v1/hardware/thermal/apply` | apply and persist a validated policy |
| `GET` | `/api/v1/activity` | current in-memory event history |

EFI + DT uses the native EDK2/BLS adapter shared by Q6A/Q8B, not U-Boot or
legacy rsetup. The running kernel's unique Type #1 entry supplies ordered
Overlay selection. Plans include `bootChange` (kernel, entry, DTB and ordered
DTBO paths) and bind all BLS entries, managed DTBO bytes and the selected base
DTB. Default boot choice and other kernel entries are never changed.

Protected ESP reads use the helper's argument-free `overlays-inspect` verb.
`requiresAuthorization` is not an empty selection. Authorized results are
cached only in memory and marked `cached`; background reads never prompt.
GPIO uses the same saved selection and exposes its kernel/cache provenance.
Root apply acquires a lock, revalidates the token, tests the combination with
`fdtoverlay`, creates a private recovery backup, and replaces the BLS entry
atomically. DTBO renames are ordered to keep old/new entry references valid;
reported failures trigger rollback. Unsupported, ambiguous or unsafe paths
fail closed. Saved configuration is distinct from the currently running mux.

Remote binding, authentication, persistent audit storage, multi-user policy,
streaming job output, and cancellation are intentionally not implied by this
pre-1.0 API. Mirror benchmarking can stop between providers; this does not imply
cancellation of state-changing operations.

## Native action boundary

The Rust control plane never invokes the legacy `rsetup` command. Fixed catalog
actions run through built-in implementations that call only the required Linux
system tools. Git history preserves upstream behavior for comparison; no legacy
runtime tree or fallback executor is shipped.

Do not migrate a hardware operation merely by translating its syntax. Preserve
its compatibility checks, recovery warning, target validation, and board test
matrix.

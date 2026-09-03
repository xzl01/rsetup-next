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
                 └──────── APT source manager ──── validated mirror catalog
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

The Linux provider currently reads only conventional local interfaces:

- `/proc/device-tree`, DMI, `/etc/os-release`, `uname`
- `/proc/meminfo`, `/proc/loadavg`, `/proc/uptime`
- `/sys/class/thermal` and `/sys/class/net`
- `df`, `ip`, and selected `systemctl is-active` probes

A failed optional probe produces an unavailable or unknown signal instead of
turning the entire device snapshot into a failure.

## Guided action execution

Each `ActionSpec` declares an identifier, category, description, risk level,
root requirement, estimate, visible procedure steps, and a fixed command. The
command is excluded from API serialization.

Execution has two independent gates:

1. `ExecutionPolicy::DryRun` is the default and produces a synthetic result.
2. Guarded or more severe actions require explicit confirmation.

Live execution additionally rejects root-required operations when the server or
CLI does not have root privileges. A production daemon should replace
process-wide root with a small privileged helper and explicit policy rules.

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

## HTTP API

The loopback server exposes:

| Method | Route | Meaning |
| --- | --- | --- |
| `GET` | `/api/v1/health` | process liveness |
| `GET` | `/api/v1/snapshot` | current device state |
| `GET` | `/api/v1/actions` | guided operation catalog |
| `POST` | `/api/v1/actions/{id}/run` | execute or dry-run one catalog action |
| `GET` | `/api/v1/sources` | detected APT source state and trusted mirror catalog |
| `POST` | `/api/v1/sources/plan` | preview managed source changes for one provider ID |
| `POST` | `/api/v1/sources/apply` | confirm and apply an exact plan token; stale plans are rejected |
| `GET` | `/api/v1/activity` | current in-memory event history |

Remote binding, authentication, persistent audit storage, multi-user policy,
streaming job output, and cancellation are intentionally not implied by this
alpha API.

## Native action boundary

The Rust control plane never invokes the legacy `rsetup` command. Fixed catalog
actions run through built-in implementations that call only the required Linux
system tools. The original Bash tree may be consulted as migration reference,
but it is not a runtime dependency or fallback executor.

Do not migrate a hardware operation merely by translating its syntax. Preserve
its compatibility checks, recovery warning, target validation, and board test
matrix.

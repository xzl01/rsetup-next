# System image installation

`rsetup-next system` lists the block devices of a running SBC and writes a
system image (`.img` or `.img.xz`) to one of them, but only after a preview that
names every partition it destroys and a confirmation that requires typing the
exact target device name. The first board this procedure targets is the Radxa
Cubie A7Z (CIX A733, UFS + eMMC + SD).

This document is the operator-facing procedure, the on-board verification
checklist, and the record of what is verified where. The gate invariants behind
the refusals are documented in [architecture.md](architecture.md); they are not
repeated here.

## Status labels used in this document

| Label | Meaning |
| --- | --- |
| **Verified locally** | Reproduced on the development host with the shipped binary: a CLI dry run, a real request against the demo HTTP server, or the automated suite. It is *not* evidence about a physical board. |
| **Covered by tests** | Exercised by the automated suite through fixtures, an injected backend, or a mock runtime — not driven by hand on this host. |
| **Pending on-board** | Must be executed on a real Cubie A7Z with a real image. No result exists yet; never report it as passing. |

A synthetic, dry-run, or fixture result never counts as hardware evidence, and a
label applies only to what was actually executed. Every claim about a surface
names its source in "Where each statement comes from".

## Command surface

```sh
rsetup-next system targets [--json]
rsetup-next system plan --target <id> --image <absolute path> [--json]
rsetup-next system install --target <id> --image <absolute path> \
  [--acknowledge <device>] [--live-execution] [--json]
```

- `targets` and `plan` are read-only previews. They need no root and write
  nothing.
- `install` previews, asks for the typed device name, and then applies.
- Without `--live-execution` on Linux, `install` remains a **dry run**: it shows
  real device identity and reports a completed run, but writes no byte.
- A live write needs root: run it under `sudo`, or run it unprivileged and let
  `pkexec` start the fixed-verb helper `/usr/libexec/rsetup-next-helper`.
- The same procedure is available from the TUI, the HTTP API, the Tauri desktop
  shell, and the browser page; see "One procedure, four surfaces" below.

## One procedure, four surfaces

All four product surfaces — CLI, TUI, Web console, desktop shell — call the same
`SystemInstallManager`, so the gates, the image validation, the plan token and
the typed-name comparison cannot drift between them. None of them keeps an
execution policy of its own: a write happens only when the controller's policy
is `Live` (CLI: `--live-execution`), and the provider (real or demo) comes from
the controller snapshot. A surface that is not live refuses the write instead of
simulating it, so a caller can never mistake a simulated write for a real one.

| Surface | Entry | Live write |
| --- | --- | --- |
| CLI | `rsetup-next system targets\|plan\|install` | `--live-execution` on Linux |
| TUI | `i` on the main screen, `Esc` returns to the dashboard | controller policy only |
| HTTP API | `GET/POST /api/v1/storage/install/…` | server started with `--live-execution` |
| Tauri desktop shell | `system_install_targets` / `system_install_plan` / `system_install_apply` | same controller policy |
| Browser page | nav **Install** in the loopback console | same server policy |

The table has five rows because the Web surface is reachable both as an HTTP API
and as a page; they share one server-side manager and one policy.

### TUI panel

Press `i` on the main screen to open the system-image panel (`Esc` closes it).
`Tab` cycles the device list, the image path, and the typed-name field; `j`/`k`
(or the arrow keys) move inside the list and `r` refreshes the device list —
those three letters act as shortcuts only while the list has focus, and are
ordinary text input in the path and confirmation fields. `Backspace` edits, and
`Enter` loads the plan from the list/path field or attempts the write from the
confirmation field. Every refusal is the core's: the panel renders the core's
error code. Its "disabled until the typed name matches" hint is display-only and
never bypasses the core. The panel defaults to never writing: demo and non-Linux
hosts use the synthetic provider, and a real target additionally needs the Live
policy, otherwise a dry-run writer discards every byte.

**Covered by tests** (render and key routing through a `TestBackend`); the panel
has not been driven by hand in this document.

### HTTP API

```text
GET  /api/v1/storage/install/targets
  → 200 {"liveAllowed": false,
         "targets": [<SystemInstallTarget>…],
         "images":  [{"id": "radxa-cubie-a7z.img.xz", "sizeBytes": 1200000000}]}

POST /api/v1/storage/install/plan
  body {"targetId": "sda", "imageId": "radxa-cubie-a7z.img.xz"}
  → 200 <SystemInstallPlan, camelCase>

POST /api/v1/storage/install/apply
  body {"targetId": "sda", "imageId": "radxa-cubie-a7z.img.xz",
        "planToken": "<from plan>", "acknowledgement": "sda"}
  → 200 {"run": …, "plan": …, "bytesWritten": 4194304,
         "verification": {"state": "matched|mismatched|unsupported", …}}
```

Rules the surface enforces before the core ever sees a request:

- The request must come from the loopback origin, and every mutating call must
  carry `x-rsetup-request: 1`. Otherwise the server answers 403
  `request_forbidden` ("Use the local console or an SSH tunnel…").
- A client never submits a path. It submits an `imageId` that must be a plain
  file name ending in `.img` or `.img.xz` inside the server's image directory
  (`/var/lib/rsetup-next/images`, overridable with `RSETUP_IMAGE_DIR`). A
  traversal component, a separator, a dot-name, a control character, a name
  longer than 255 bytes, a symlink, a directory, or an absent file is rejected
  with 400 `invalid_install_request`. An extra `imagePath` field in the body is
  ignored: the resolved path always comes from the catalogue.
- `plan` echoes the catalogue id back as `image.id` next to the resolved
  `request.imagePath`, so a client can build its apply payload from the plan
  response alone; the browser page uses that echoed id as its only source. The
  token binding is computed from the resolved path, size and hash — not from this
  label — so the echoed id cannot widen what may be written.
- The catalogue lists only the ids `apply` would accept (same rules). A file that
  passes those rules but has no MBR/GPT partition table is listed and then
  rejected by `plan` — every device-time decision stays with the core.
- `liveAllowed` is false unless the server was started with `--live-execution`.
  In that case an otherwise valid `apply` is refused with 422
  `install_unsupported` and "…Nothing was written.", because the surface refuses
  its own write rather than reporting a simulated one.

Errors use the same envelope as the rest of the API,
`{"error":{"code":"…","message":"…"}}`. The status for each code is in the
failure table below; `root_required` is 403 and the write-time `internal_error`
is 500. A busy server can also answer 429 `server_busy`.

**Verified locally** against a real demo server on 2026-10-13 (the exact run is
recorded below and in
[docs/testing/system-install-2026-10-13/report.md](testing/system-install-2026-10-13/report.md)).

```console
$ RSETUP_IMAGE_DIR=/tmp/rsn-images rsetup-next --demo serve --listen 127.0.0.1:8799
$ curl -s $B/api/v1/storage/install/targets
  HTTP 200 · liveAllowed=false · images: demo.img, not-an-image.img (link.img excluded)
$ curl -s -H 'x-rsetup-request: 1' -d '{"targetId":"sda","imageId":"demo.img"}' .../plan
  HTTP 200 · request.imagePath=/private/tmp/rsn-images/demo.img · image.id=demo.img
$ .../apply {"acknowledgement":"mmcblk0"}
  HTTP 409 {"error":{"code":"acknowledgement_mismatch","message":"type \"sda\" to write /dev/sda; received \"mmcblk0\""}}
$ .../apply {"acknowledgement":""}
  HTTP 409 {"error":{"code":"confirmation_required","message":"confirmation is required before writing a system image"}}
$ .../apply {"acknowledgement":"sda"}                       # server not started live
  HTTP 422 {"error":{"code":"install_unsupported","message":"Live execution is not enabled on this surface; restart with --live-execution to write an image. Nothing was written."}}
$ .../apply with the same token again
  HTTP 409 {"error":{"code":"stale_plan","message":"the target or the image changed after preview; create a fresh plan"}}
$ .../plan {"imageId":"../demo.img"} | {"imageId":"link.img"} | {"imageId":"not-an-image.img"}
  HTTP 400 invalid_install_request ("…plain file name, not a path" / "…regular file, not a symlink or a directory" / "…no MBR or GPT partition table…")
$ .../plan {"targetId":"mmcblk1"}
  HTTP 409 {"error":{"code":"writes_running_system","message":"refusing to write /dev/mmcblk1: it hosts the running root or boot filesystem"}}
$ .../plan without the x-rsetup-request header
  HTTP 403 {"error":{"code":"request_forbidden","message":"Use the local console or an SSH tunnel; cross-origin requests are forbidden."}}
```

### Tauri desktop shell

Three commands, called with the same payloads as HTTP:

| Command | Arguments (JS `invoke` keys) | Returns |
| --- | --- | --- |
| `system_install_targets` | none | `{liveAllowed, targets, images}` |
| `system_install_plan` | `targetId`, `imageId` | `SystemInstallPlan` |
| `system_install_apply` | `targetId`, `imageId`, `planToken`, `acknowledgement` | `SystemInstallApplyResult` |

Failures are returned as `{code, message}` with the same ten codes the CLI and
HTTP use (the crate mirrors the CLI's table and says so in a comment above the
conversion). Only `system_install_targets` is classified as read-only: it takes
no arguments and mutates nothing, while plan and apply are deliberately excluded
from that list. Without live execution the shell refuses the write exactly like
HTTP (the same refusing writer), so a demo or preview session cannot write.

**Covered by tests**: the commands dispatch through the mock runtime, apply
requires an acknowledgement, and the registry guard fails if a command is
dropped. The desktop application has not been launched and driven by hand for
this document.

### Browser page

The loopback console gains an **Install** route (`data-route="install"` →
`<section class="view" data-view="install">`) with a refresh button for the
catalogue. It:

- reads `GET /api/v1/storage/install/targets` (or `system_install_targets` when
  embedded in Tauri) and renders a `data-live-allowed="true|false"` banner —
  when live writes are off it states that a confirmed write is refused with
  `install_unsupported` and that nothing is written;
- offers every detected device but disables the ones that carry the running
  system or are mounted, with the reason in the option label, and lists image
  catalogue ids only (never a free-text path);
- renders the whole plan: target identity, the destroyed partitions with their
  mount points, the image format/sizes/SHA-256 prefix/partition table, the two
  mandatory warnings plus the core's own warnings, and the authorization notice;
- submits the typed device name as the `acknowledgement` string in
  `{targetId, imageId, planToken, acknowledgement}` — never a boolean and never
  an `imagePath` — where the `imageId` is the id the reviewed plan echoed back,
  so the identity shown and the identity submitted cannot diverge; the apply
  button's disabled state comes from a display-only predicate that mirrors the
  core's trimmed comparison;
- renders one message per stable code (ten codes plus a generic fallback) so a
  failure never collapses into a single generic sentence, and treats
  `stale_plan`/`plan_required` by discarding the plan and asking for a fresh one.

**Covered by tests**: `node --test ui/*.test.mjs` → 88 passed, 0 failed,
including the twelve cases of `ui/system-install.test.mjs` (consequence list,
typed-name gating, payload shape and its single image-id source, per-code copy,
stale-plan reset, live-disabled refusal). The page has not been clicked through
by hand in this document.

## What the preview prints

```text
$ rsetup-next system plan --target sda --image /root/radxa-cubie-a7z.img.xz
System image write preview
Target device: /dev/sda — UFS (RADXA UFS 128GB)
Capacity: 128.00 GiB
Existing partitions destroyed (3):
  - sda1 · 128.00 MiB
  - sda2 · 2.00 GiB
  - sda3 · 6.00 GiB
Image: /root/radxa-cubie-a7z.img.xz
  Format: Xz · File size: 768 B · Write size: 4.00 MiB
  SHA-256 (first 1 MiB): 2e835412001233bd54b817d2f388a596e1857afe6b817502ff038c8356b6a215
  Partition table: not read (compressed image)
Administrator authorization required
! Writing replaces every partition on the target.
! This write cannot be rolled back.
! The image is compressed and is decompressed while writing (requires xz).
```

The numbers of that example come from a demo target and a four-megabyte fixture,
so treat the shape as the contract, not the values. What matters:

- **Target identity** — path, kind (`UFS`, `eMMC`, `SD card`, `NVMe`), model,
  capacity, plus `Removable media`, `Mounted <points>` and
  `Hosts the running system` markers when they apply.
- **Destroyed partitions** — name, size, and mount point for every partition on
  the target. The list is the whole consequence of the write.
- **Image** — format, compressed file size, uncompressed **write size** (for
  `.img.xz` this comes from the stream's index, not from the file size), and the
  SHA-256 of the first 1 MiB, which the plan token binds.
- **Partition table** — `MBR`, `GPT`, or `not read (compressed image)`.
- **Warnings** — always "replaces every partition" and "cannot be rolled
  back"; additionally "removable media" or "requires xz" when they apply.

`--json` returns the same plan as a machine-readable object with `planToken`,
`target`, `image`, `destroys`, `warnings`, `requiresRoot` and `chunkBytes`.
Success is printed on stdout; a refusal prints `{"error":{"code":…,"message":…}}`
and exits 1.

## Failure codes

Every surface reports one of these stable codes, so a script can branch on the
code instead of parsing prose. The CLI prints `Error: <code>: <message>` and
exits 1; the HTTP API returns the code in its error envelope with the status
below; the Tauri commands return `{code, message}`; the browser page renders its
own copy per code. The vocabulary is defined once, in
`crates/rsetup-app/src/main.rs::system_install_error_code`, and the desktop crate
mirrors it.

| Code | HTTP | Meaning | What to do |
| --- | --- | --- | --- |
| `confirmation_required` | 409 | The typed device name was empty (for example stdin was closed). | Type the name, or pass `--acknowledge <device>`. |
| `acknowledgement_mismatch` | 409 | The typed name is not the target's device name. | Re-read the preview; the message shows the expected and the provided value. |
| `writes_running_system` | 409 | The target carries the running root or boot filesystem. | Boot another medium and write from there. There is no override. |
| `install_conflict` | 409 | The target is mounted, or the image lives on it. | Unmount the target; move the image elsewhere. |
| `invalid_install_request` | 400 | Relative or unreadable image path, an image id that is not a plain catalogue name, symlink, directory, empty file, no MBR/GPT, missing target, unknown device, image larger than the target. | Fix the request; the message names the exact problem. |
| `install_unsupported` | 422 | The step is impossible here: the host cannot re-read partition tables, `xz` is missing for a `.img.xz`, or the surface was not started for live execution. | Use Linux; install `xz-utils`; or enable live execution. Nothing was written. |
| `plan_required` | 409 | `apply` was called without a plan token. | Run `plan` first. |
| `stale_plan` | 409 | The token expired (600 s), was already consumed by the process that issued it, or the target/image changed. | Preview again and use the new token. |
| `root_required` | 403 | A live write was requested without root and without the helper. | Use `sudo`, or install the package that ships the helper. |
| `internal_error` | 500 | I/O or device error during the write. | Read the message: it reports how many bytes were written. |

The HTTP boundary can also answer 403 `request_forbidden` before any of these
codes applies (cross-origin, non-loopback, or a mutating call without the
`x-rsetup-request: 1` marker), and 429 `server_busy` when too many operations
run at once.

The HTTP column comes from the surface's own mapping
(`ApiError::from_install` plus the live-refusal override). Every code quoted in
the HTTP example above was observed on the demo server; codes the demo targets
cannot produce — a mounted-but-not-booting target (`install_conflict`) or
`root_required` — are documented from that mapping alone.

## How the plan token works

A plan token is a **handle, not a credential**:

- It is single use and expires 600 s after issuance, and both are decided by the
  **issuing process's record** of what it handed out — not by anything a caller
  writes into the token. A token another process minted is refused outright,
  whatever its fields say.
- Editing a token field (binding, issuance time, nonce or signature) makes it
  fail verification, so a consumed token cannot be made to look new and an
  expired one cannot be re-armed.
- A surface that previews in one process and applies in another — the privileged
  helper — uses `verify_preview_token` to notice that the device state moved
  since the preview the operator was shown, or that the preview is old. Because
  the caller supplies the issuance time, **that cross-process check is
  advisory**: it guards against mistakes and re-runs, and is not an adversarial
  boundary.
- What actually protects the device is everything else: the plan is rebuilt from
  the current state and its binding must match, the hard gates are re-evaluated,
  the device name must be typed, and a real write needs administrator
  authorization.

## Where each statement comes from

| Statement in this document | Source (checked 2026-10-14) |
| --- | --- |
| CLI commands, dry run by default, helper path, error codes and exit status | `crates/rsetup-app/src/main.rs` (`SystemCommands`, `run_system_command`, `system_install_error_code`) |
| TUI panel entry keys, focus routing, display-only predicate, dry-run writer | `crates/rsetup-app/src/tui.rs` (`InstallPanel`, `App::install_key`, `install_plan_lines`) |
| HTTP routes, response fields, catalogue rules, boundary header, status mapping, live refusal | `crates/rsetup-app/src/server.rs` (router lines 202-204; `InstallOptions`/`InstallPlanRequest`/`InstallApplyRequest`; `catalog_image_path`; `InstallSurface::install_error`; `ApiError::from_install`) |
| Tauri command names, arguments, read-only classification, mirrored codes | `apps/desktop/src-tauri/src/main.rs` (`system_install_targets|plan|apply`, `generate_handler!`, `IPC_COMMANDS`, `READ_ONLY_COMMANDS`, `CommandError`) |
| Browser page entry, payloads, disabled predicate, per-code copy | `ui/index.html` (nav `data-route="install"`, `data-view="install"`), `ui/app.js` (`install*` helpers from line 2841 and the view handlers), `ui/i18n.js` (`install.*` keys) |
| Gates, image validation, typed-name comparison, partition re-read | `crates/rsetup-core/src/system_install.rs`; invariants in [architecture.md](architecture.md) |
| Token model: handle not credential, per-process issuance record, HMAC, single use, 600 s window, advisory cross-process preview check | `crates/rsetup-core/src/system_install.rs` (module section "Tokens: what is guaranteed, and by what"; `SystemInstallManager::{plan, apply_live, consume, verify_preview_token}`) |
| Two kinds of unsupported verification (compressed image vs host that cannot re-read) | `crates/rsetup-app/src/tui.rs` (`verification_text`), `crates/rsetup-app/src/main.rs` (`write_install_result`), `crates/rsetup-app/src/i18n.rs` (`install_verification_compressed_not_compared`) |
| `xz -dc` decompression and the missing-tool reason | `crates/rsetup-core/src/system_install.rs` (`SystemImageOpener::open`) |
| Test and endpoint results quoted here | the commands in this document: `cargo test --workspace --locked`, `node --test ui/*.test.mjs`, and the demo-server `curl` run reproduced below |

## Packaging: the xz-utils dependency

`.img.xz` is the format Radxa publishes, so decompression is part of the main
installation workflow. The package declares a hard `Depends: xz-utils`; the
trade-off (and why not `Recommends`) is recorded in
[debian/README.source](../debian/README.source).

Absence is handled without a crash and without a silent short write:

- **Preview works without `xz-utils`** — the `.xz` header, index and footer are
  parsed in Rust, so the write size, the SHA-256 binding, and the
  "requires xz" warning are all produced without the tool.
  **Verified locally** (fixture `.img.xz`, preview exits 0).
- **The write itself needs it.** With `xz` absent from `PATH`:

  ```console
  $ env PATH=/usr/bin:/bin rsetup-next --demo system install \
      --target sda --image image.img.xz --acknowledge sda --json
  {"error":{"code":"install_unsupported","message":"system image installation is unavailable: install xz-utils to write a compressed image"}}
  $ echo $?
  1
  ```

  **Verified locally** on a host with `xz` removed from `PATH`.

`debian/tests/smoke` asserts the packaged binary exposes the new surface
(`system targets --json`) and keeps reporting a stable code when a request is
refused. **Verified locally** by running both lines.

## On-board verification checklist: Radxa Cubie A7Z

This is the checklist a human executes on the board. Everything in this section
is **Pending on-board** unless a step says otherwise. Record the output of each
step; a step without recorded output is not a passing step.

Every step below uses the CLI. A live write through the TUI, the loopback Web
console, or the desktop shell needs live execution enabled on the board and is
not part of this procedure; those paths stay **Pending on-board** as well.

### Step 0 — prerequisites

- Cubie A7Z powered from its own supply, serial console or SSH available, and a
  **second** boot medium (SD card or eMMC) so the write target is never the
  running system. Never plan to recover by pulling the power.
- A known recovery path: Radxa maskrom / the board's documented flashing mode,
  plus the tool needed to re-flash the medium if the write is interrupted.
- The installed package: `rsetup-next --version` prints `rsetup-next 0.5.0` or
  newer, and `/usr/libexec/rsetup-next-helper` exists.
- An image and its published SHA-256:

  ```sh
  ls -l /root/radxa-cubie-a7z_*.img.xz
  sha256sum /root/radxa-cubie-a7z_*.img.xz
  xz -t /root/radxa-cubie-a7z_*.img.xz   # silent, exit 0
  curl -fsSLO <the published SHA256SUMS or .sha256 file>
  sha256sum -c <published checksum file>
  ```

  **Failure criteria:** checksum mismatch, `xz -t` reporting corruption, or an
  image larger than the target's capacity — stop, do not write.

### Step 1 — identify the candidates

```sh
rsetup-next system targets
rsetup-next system targets --json
```

Expected: one line per candidate with kind, model, capacity and the destroyed
partition count. On an A733 board the UFS controller enumerates as an `sd*`
node; trust the **kind** column (`UFS`), not the node name. The SD card the
system booted from must show `Removable media`, its mount points, and
`Hosts the running system`:

```text
/dev/mmcblk1 — SD card SD 32GB · 29.00 GiB · 2 Existing partitions destroyed · Removable media · Mounted /, /boot · Hosts the running system
```

**Verified locally** (synthetic devices, macOS): the listing above, in both text
and JSON, with no root and no device access.
**Pending on-board:** that the real UFS/eMMC appear with the correct kind, size
and partition list, and that the boot medium is marked as hosting the running
system.

**Failure criteria:** a UFS or eMMC device missing, a wrong `kind`, a mounted
target not marked `Mounted …`, or the boot medium not marked
`Hosts the running system`. Any of these must be reported before writing.

### Step 2 — preview the write (no root, nothing is written)

```sh
rsetup-next system plan --target sda --image /root/radxa-cubie-a7z_*.img.xz
rsetup-next system plan --target sda --image /root/radxa-cubie-a7z_*.img.xz --json
```

Expected: the preview shown earlier, with **your** device and image values, and
these lines present:

- `Write size:` equal to the uncompressed image size (larger than the file
  size), never `0 B`;
- `Partition table: not read (compressed image)`;
- `! The image is compressed and is decompressed while writing (requires xz).`;
- exit code 0, and no write to any device.

Then repeat for the eMMC (`--target mmcblk0`).

**Failure criteria:** a non-zero exit, a missing destructive warning, a write
size larger than the target capacity, or a device in the preview that is not the
one you intend to overwrite. Save the `planToken` if you plan to use `--json`
plus an out-of-band apply.

### Step 3 — write the target (irreversible)

```sh
sudo rsetup-next --live-execution system install --target sda \
  --image /root/radxa-cubie-a7z_*.img.xz --acknowledge sda
```

The typed name must be exactly the target's device name (`sda`, not
`/dev/sda`, not `SDA`, not a partition name). The acknowledgement is checked by
the core, not by the CLI, so the same rule holds for the TUI and API later.

Expected output: the preview, then the write report:

```text
Write complete
  Target device: /dev/sda
  Bytes written: <the uncompressed image size>
  Partition table verification: <see below>
```

- For a **raw `.img`** the last line reads
  `the re-read partition table matches the image`. **Pending on-board** (the
  Linux `BLKRRPART` re-read cannot run on the development host, where the line
  reads `this host cannot re-read partition tables`).
- For a **compressed `.img.xz`** the layout is never read before writing, so
  there is nothing to compare: the run reports its verification state as
  `unsupported` and names that reason, rather than claiming a mismatch. The CLI
  and the TUI distinguish the two cases — a compressed image prints the
  compressed-image copy plus the core's reason, while a host that cannot re-read
  tables at all (a non-Linux development host) prints the generic
  "cannot re-read" reason. In JSON the same facts are
  `verification.state = "unsupported"` with a `detail` explaining that the
  compressed image's layout was not parsed. This is **expected, not a failure**;
  confirm success in step 4 instead. **Pending on-board**, because the exact text
  can only be observed on Linux against a real device.

**Failure criteria:** any non-zero exit; a byte count different from the
preview's write size; a write that stops mid-way (the error then reports
`wrote N of M bytes before failing (…); the target now holds a partial image and
cannot be rolled back`); a kernel log full of I/O errors; or the board losing
the target device after the write.

### Step 4 — verify the medium before rebooting

```sh
lsblk -o NAME,SIZE,TYPE,FSTYPE,LABEL,MOUNTPOINTS
sudo blkid
sudo partprobe /dev/sda     # optional: ask the kernel to re-read
sudo cmp -n 4194304 /root/<image>.img /dev/sda   # raw images only
```

Expected: the partition table the image declares, with the same sizes as the
preview's destroyed partitions (now replaced), and — for a raw image — `cmp`
reporting no difference for the compared prefix.

**Failure criteria:** no partition table, partition sizes that disagree with the
image, or a filesystem that cannot be read. Do not reboot into it; re-flash.

### Step 5 — boot the written medium

1. Power off, remove the other boot medium, and let the board boot the freshly
   written UFS/eMMC.
2. On the board: `findmnt /`, `cat /etc/os-release`, `uptime`.
3. Repeat step 1 from the new system and confirm the device it now boots from is
   the one you wrote, and that the old boot medium is no longer marked
   `Hosts the running system`.

**Failure criteria:** no boot, an unexpected OS, or a root filesystem that does
not match the image. Recover with the documented flashing mode.

### Step 6 — negative checks (run these before the destructive write)

These prove the gates work on the real board. Use the **expendable** boot medium
for the last one, or run it before step 3 while the target is still untouched.

```sh
# 1. running-system device must be refused, permanently, with no override
rsetup-next system plan --target mmcblk1 --image /root/<image>.img.xz ; echo $?
# → Error: writes_running_system: refusing to write /dev/mmcblk1: it hosts the running root or boot filesystem
#   exit 1

# 2. a wrong typed name must be refused after the preview
sudo rsetup-next --live-execution system install --target sda \
  --image /root/<image>.img.xz --acknowledge mmcblk0 ; echo $?
# → Error: acknowledgement_mismatch: type "sda" to write /dev/sda; received "mmcblk0"
#   exit 1

# 3. an empty answer must be refused
sudo rsetup-next --live-execution system install --target sda \
  --image /root/<image>.img.xz < /dev/null ; echo $?
# → Error: confirmation_required: confirmation is required before writing a system image
#   exit 1

# 4. a mounted target must be refused and its mount points listed
#    (mount one partition of the expendable medium first)
sudo mount /dev/sdb1 /mnt
rsetup-next system plan --target sdb --image /root/<image>.img.xz ; echo $?
# → Error: install_conflict: … plus the mount points
sudo umount /mnt
```

**Verified locally** (macOS, demo backend): the exact messages and exit codes of
checks 1–3, and the refusal of relative paths, missing files, directories and
symlinks, images without a partition table, unknown device ids, and
`--live-execution` on a non-Linux host
(`Error: live execution is only supported on Linux SBC hosts`).

**Pending on-board:** that checks 1–4 hold against real sysfs and `/proc/mounts`
data, and that no byte is written when a check refuses. Prove the "no byte"
claim by sampling the target before and after:

```sh
sudo dd if=/dev/sda bs=1M count=4 status=none | sha256sum   # before
# … run a refused command …
sudo dd if=/dev/sda bs=1M count=4 status=none | sha256sum   # must be identical
```

**Failure criteria for any gate:** a refusal that exits 0, a refusal with the
wrong code, a message that does not name the device, or any change to the first
4 MiB of a target that was supposed to be refused.

### Recording the result

Write the run up as `docs/testing/system-install-<date>/report.md`, following the
existing reports in `docs/testing/`: board and SoC, image name and SHA-256, the
exact commands, the observed output of every step, the gates exercised, the
failures, and an explicit list of what was **not** covered. A checklist step
without output is not evidence.

## What is verified locally today

| Area | Status | Evidence |
| --- | --- | --- |
| Candidate listing (text and JSON) | **Verified locally** | `--demo system targets` prints four labelled devices; no root, no device node. |
| Preview: target, destroyed partitions, SHA-256, warnings | **Verified locally** | `system plan` against a 4 MiB MBR fixture: `Partition table: MBR`, one partition per entry, both mandatory warnings. |
| Preview of `.img.xz` (write size from the index, "requires xz" warning) | **Verified locally** | `system plan` against an LZMA-compressed fixture; `Write size: 4.00 MiB` while the file is 768 B. |
| Dry run writes nothing | **Verified locally** | `--demo system install … --acknowledge sda` reports the write complete and prints the synthetic notice; `--demo` never reaches a device. |
| Typed-name gate and empty answer | **Verified locally** | `acknowledgement_mismatch` / `confirmation_required`, exit 1, message contains expected and provided values. |
| Running-system and mounted-target gates | **Verified locally** | `writes_running_system` for the demo SD card that hosts `/` and `/boot`; injected-root unit tests for mounts. |
| Request validation (path, symlink, empty, no MBR/GPT, unknown device, oversize) | **Verified locally** | Each case exits 1 with `invalid_install_request` and a specific message. |
| Missing `xz-utils` | **Verified locally** | `install_unsupported` + "install xz-utils to write a compressed image" with `xz` removed from `PATH`. |
| Core behaviour | **Verified locally** | 22 `system_install` unit tests plus the workspace suite (`cargo test --workspace --locked`) — all injected backends, no real device node. |
| HTTP contract (routes, payloads, statuses, catalogue and boundary rules) | **Verified locally** | Demo server on 2026-10-13 (`--demo serve --listen 127.0.0.1:8799`, `RSETUP_IMAGE_DIR`): targets 200, plan 200, wrong/empty name 409, valid name without live 422, replayed token 409, traversal/symlink/non-partitioned id 400, running-system target 409, missing `x-rsetup-request` 403. |
| TUI panel (entry, focus routing, consequence render, core-decided confirmation) | **Covered by tests** | `cargo test --workspace --locked` (app suite), including render and key-routing cases with a `TestBackend`. Not hand-driven here. |
| Tauri commands (`system_install_targets/plan/apply`) | **Covered by tests** | Desktop crate tests: dispatch through the mock runtime, acknowledgement required, registry guard intact. The GUI was not launched here. |
| Browser page (consequence list, typed-name gating, payload shape, per-code copy) | **Covered by tests** | `node --test ui/*.test.mjs` → 88 passed, 0 failed (twelve cases in `ui/system-install.test.mjs`). The page was not clicked through here. |
| Real write to UFS/eMMC on a Cubie A7Z | **Pending on-board** | No hardware run exists. |
| Live write through the TUI, HTTP, Tauri, or Web surfaces on a board | **Pending on-board** | All four need live execution enabled on the host; never exercised on hardware. |
| `BLKRRPART` re-read of a real device, and the post-write verification line | **Pending on-board** | Not executable on the development host. |
| Removable-media warning, mount conflict against real `/proc/mounts` | **Pending on-board** | Detected by injected fixtures only. |
| Recovery after an interrupted write | **Pending on-board** | The "no rollback" behaviour is tested with a failing fixture writer; the real recovery procedure is untested. |

## Device-level testing in CI

Automated tests for this feature are **fixture-level only**, and the
hardware-dependent steps above must never be turned into CI gates:

- CI runners are unprivileged containers or VMs. Attaching a loop device needs
  root and a block-device-capable host, and the project has no real SBC, USB
  medium or UFS/eMMC target in CI. A test that claimed to "write an image" would
  be writing to a file, not a device.
- macOS, the development host, has no `losetup` at all, and `BLKRRPART` is a
  Linux ioctl.
- The tests therefore drive an injected device root (`/sys/class/block`,
  `/proc/mounts`, `/dev/<name>` under a fixture directory), an injected image
  opener, and synthetic backends. Fixture roots cannot reach a real `/dev`.
- `debian/tests/smoke` stays superficial on purpose: it asserts the packaged CLI
  exposes the surface and re-reports stable error codes in demo mode. No step
  touches a block device.

Consequently: a green CI run proves the gates, the parsing and the streaming
logic, and nothing about a real board. Hardware results are recorded in
`docs/testing/`, and only then may a row of the table above be moved to
"verified on hardware".

## Reproducing the host-side checks

Everything marked **Verified locally** can be re-run without a board or root:

```sh
# build once
cargo build -p rsetup-next --locked

# read-only surface
./target/debug/rsetup-next --demo system targets
./target/debug/rsetup-next --demo system targets --json

# a throwaway MBR fixture, then preview and dry-run it
python3 - <<'PY'
import struct
buf = bytearray(4 * 1024 * 1024)
e = bytearray(16); e[4] = 0x83
struct.pack_into('<I', e, 8, 2048); struct.pack_into('<I', e, 12, 1024)
buf[446:462] = e
buf[510:512] = b'\x55\xaa'
open('/tmp/fixture.img', 'wb').write(bytes(buf))
PY
./target/debug/rsetup-next --demo system plan --target sda --image /tmp/fixture.img
./target/debug/rsetup-next --demo system install --target sda --image /tmp/fixture.img --acknowledge sda

# the missing-xz path (any PATH without xz)
env PATH=/usr/bin:/bin "$PWD/target/debug/rsetup-next" --demo system install \
  --target sda --image /tmp/fixture.img.xz --acknowledge sda --json

# the gates and the validation cases
./target/debug/rsetup-next --demo system install --target sda --image /tmp/fixture.img --acknowledge mmcblk0
./target/debug/rsetup-next --demo system install --target sda --image /tmp/fixture.img < /dev/null
./target/debug/rsetup-next --demo system install --target mmcblk1 --image /tmp/fixture.img --acknowledge mmcblk1
./target/debug/rsetup-next --demo system plan --target sda --image relative.img
./target/debug/rsetup-next --demo --live-execution system install --target sda --image /tmp/fixture.img --acknowledge sda

# the surface tests
node --test ui/*.test.mjs

# the HTTP surface against a real demo server (the run quoted above)
mkdir -p /tmp/rsn-images && cp /tmp/fixture.img /tmp/rsn-images/demo.img
RSETUP_IMAGE_DIR=/tmp/rsn-images ./target/debug/rsetup-next --demo serve --listen 127.0.0.1:8799 &
curl -s http://127.0.0.1:8799/api/v1/storage/install/targets
curl -s -H 'x-rsetup-request: 1' -H 'content-type: application/json' \
  -d '{"targetId":"sda","imageId":"demo.img"}' \
  http://127.0.0.1:8799/api/v1/storage/install/plan
```

`cargo test --workspace --locked` runs the automated suite; the `system_install`
tests in `crates/rsetup-core`, the CLI/TUI/HTTP tests in `crates/rsetup-app`, the
desktop-crate command tests, and the Node tests for the browser page are the
regression net for this feature.

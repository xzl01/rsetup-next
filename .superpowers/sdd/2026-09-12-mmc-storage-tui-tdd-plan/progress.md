# SDD ledger — plan: docs/superpowers/plans/2026-09-12-mmc-storage-tui-tdd-plan.md

## Pre-flight Plan Scan
| Pair / Task | Producible vs Consumable | Finding / Status |
| --- | --- | --- |
| Task 1.1 / Task 1.2 | `model.rs` defines `MmcHealth`, `MmcDevice`, `MmcStatus`, `StorageStatus` -> `mmc.rs` parser consumes them | Consistent |
| Task 1.2 / Task 2.1 | `mmc.rs` parser -> `MmcManager::probe_and_init` uses parsed health in constructing `MmcDevice` | Consistent |
| Task 2.1 / Task 2.2 | sysfs probing fallback to direct ioctl | Clean separation |
| Task 2.1 / Task 3.1 | `MmcManager` -> `Controller` manages `Arc<MmcManager>` | Consistent |
| Task 3.1 / Task 3.2 | Controller methods (`mmc_status`, `storage_status`) -> CLI subcommands (`mmc`, `storage`) | Consistent |
| Task 3.1 / Task 4.2 | Controller methods -> TUI App fields | Consistent |
| Task 4.1 / Task 4.3 | `i18n.rs` storage keys -> `render_storage_summary` uses keys | Consistent |
| Task 4.2 / Task 4.3 | TUI App `mmc_status` / `storage_status` -> `render_storage_summary` widget rendering | Consistent |

Plan scan is clean. Pre-flight passed.

Task 1.1: complete (commits 6752403..ebc4f69, review clean)
Task 1.2: complete (commits ebc4f69..505ae81, review clean)
Task 2.1: fix round 1/5 (1 addressed, 0 open — primary block node filtering; commits e341310..9237ef6)
Task 2.1: complete (commits 505ae81..9237ef6, review clean)
Task 2.1: minor (deferred): regression test covers block/ dir path only; dev_dir fallback path not directly tested
Task 2.1: minor (deferred): multiple primaries in one dir still first-match (unrealistic on real sysfs)
Task 2.2: complete (commits 9237ef6..aa5fa98, review clean)
Task 2.2: minor (deferred): sys.rs:41 comment typo "CMD6 — SEND EXT_CSD" should be CMD8 (constant value is correct)
Task 3.1: complete (commits aa5fa98..6ec422f, review clean)
Task 3.2: complete (commits 6ec422f..5cb92c1, review clean)
Task 3.2: minor (deferred): "Type:" EN label not in brief (implementer's choice consistent); storage-section order not asserted in test
Ruling (note): correct test package is `cargo test -p rsetup-next` (crates/rsetup-app package name is rsetup-next), not `cargo test -p rsetup-app` — use in all later briefs
Task 4.1: complete (commits 5cb92c1..7f69454, review clean)
Task 4.2: complete (commits 7f69454..4e61103, review clean)
Task 4.3: complete (commits 4e61103..98735dc, review clean; 4 implementer deviations all adjudicated acceptable — wrapped-row height accounting proven required by brief's own 100x40 test)
Task 4.3: minor (deferred): wrapped_rows char-ceil can under-count only if a future line mixes wide CJK with trailing ASCII (current lines all ASCII; report flags it)
Task 4.3: minor (deferred): wrapped_rows doc comment rationale imprecise (ratatui word-wraps, not char-wraps; numbers match for current data)
Task 4.3: minor (deferred): pre-existing NVMe labels ("状态: " etc.) copied verbatim per Ruling 1, still hardcoded zh/en instead of locale.text(storage_*); grandfathered, i18n keys exist if a later change wants them

## Final
Stage 5 verification: cargo test --workspace 117+38+1 passed/0 failed; clippy clean; build clean; CLI smoke OK (hardware mmc zh, hardware storage --json).
Final whole-branch review: Ready to merge. Only must-fix was sys.rs:41 CMD6→CMD8 comment — fixed in d02a97b; second stale CMD6 comment at line 58 fixed in 5b14b73.
Follow-ups (non-blocking): REST/Web exposure of /hardware/storage; oemid/date metadata; NVMe labels via storage_* dict (consumes 12 idle keys); probe_and_init error-tolerance symmetry; format_storage_status NVMe-first order assertion.
Hygiene: 7db6429 untracked accidentally-committed SDD report + added .superpowers/ to .gitignore.
All 10 plan tasks complete (commits ebc4f69..5b14b73 on top of docs commit 6752403).

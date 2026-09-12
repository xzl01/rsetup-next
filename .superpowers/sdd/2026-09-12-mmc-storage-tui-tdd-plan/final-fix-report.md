# Final Fix Report — whole-branch review must-fix

**Branch:** `dev-aghost` — multi-drive MMC + unified Storage TUI
**Status:** DONE
**Commit:** `d02a97b` — `docs(core): correct SEND EXT_CSD command number comment (CMD8)`

## Fix applied

- File: `crates/rsetup-core/src/mmc/sys.rs`, line 41
- Change: doc comment `/// CMD6 — SEND EXT_CSD (include/linux/mmc/core.h).` → `/// CMD8 — SEND EXT_CSD (include/linux/mmc/core.h).`
- No code, constants, or tests were touched (verified via `git diff` — single-line comment change only).
- `MMC_OPCODE_SEND_EXT_CSD: u32 = 8` was already correct and left unchanged.

## Verification

- `PATH="/usr/bin:$PATH" cargo test -p rsetup-core mmc` → **ok. 15 passed; 0 failed; 104 filtered out** (unchanged from baseline).

## Note for the reviewer (not changed, per instructions)

Line 58 of the same file still contains the same stale reference: `/// Parsed subset of the 512-byte EXT_CSD register (CMD6).` The fix instruction scoped the change to line 41 only, so this was intentionally left untouched. Recommend a follow-up doc fix to make it `CMD8`.

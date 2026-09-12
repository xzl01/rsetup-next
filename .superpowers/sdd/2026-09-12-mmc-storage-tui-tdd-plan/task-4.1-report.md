# Task 4.1 Report: TUI 存储字典键扩展 (i18n.rs)

## Status: DONE

## Commit
- `7f69454` — `feat(app): add unified storage TUI dictionary keys` (branch `dev-aghost`)
- 单文件改动：`crates/rsetup-app/src/i18n.rs`，+103 行，未触碰任何渲染逻辑 / tui.rs / 其他文件。

## Changes
1. `Locale::text` 中文块：在 `nvme_not_detected` (ZhCn) 之后新增 29 个 `storage_*` 键（`storage_telemetry` … `storage_written`），格式与缩进与现有 NVMe 中文键一致，中文值不带句末标点。
2. `Locale::text` 英文块：在 `nvme_not_detected` (通配) 之后新增对应 29 个 `storage_*` 键；`storage_not_detected` 英文值带句号 `No NVMe or MMC storage devices detected.`（多行 arm，参照 `nvme_not_detected` 英文的写法）。
3. `#[cfg(test)]` 中紧挨 `test_nvme_tui_dictionary_keys` 之后新增 `test_storage_tui_dictionary_keys`，结构与 NVMe 测试完全一致（29 个 (key, zh, en) 三元组，双 locale 断言）。

## TDD 过程
1. 先加测试：`cargo test -p rsetup-next i18n` → `test_storage_tui_dictionary_keys` FAILED（`left: ""` vs `right: "存储状态"`，符合预期红）。
2. 再加键：`cargo test -p rsetup-next i18n` → 10 passed, 0 failed（含既有 `test_nvme_tui_dictionary_keys`，无回归）。
3. 全 crate：`cargo test -p rsetup-next` → 34 passed + helper 1 passed，0 failed，无回归。

## Test summary
`cargo test -p rsetup-next`：34 passed; 0 failed（i18n 子集 10/10 通过）。

## Concerns
无。

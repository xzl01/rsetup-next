# Task 4.1 Brief: TUI 存储字典键扩展 (i18n.rs)

## Background
`crates/rsetup-app/src/i18n.rs` 的 `Locale::text(self, key: &str) -> &'static str` 是一个 `match (self, key)` 函数：
- 中文分支为 `(Self::ZhCn, "key") => "..."`，集中出现在文件前半部分（约 123-190 行，NVMe 键在 184-190 行）。
- 英文分支为 `(_, "key") => "..."` 通配，集中出现在后半部分（约 191-262 行，NVMe 键在 254-262 行）。
- 已有 NVMe 字典键：`nvme_telemetry`、`nvme_healthy`、`nvme_warning`、`nvme_endurance`、`nvme_spare`、`nvme_io`、`nvme_not_detected`。
- 已有测试 `test_nvme_tui_dictionary_keys`（约 582 行）遍历 (key, zh, en) 三元组断言 `Locale::ZhCn.text(key)` 与 `Locale::En.text(key)`。

本任务为统一“存储 (Storage)”TUI 视窗新增一批字典键，供后续任务 4.3 的 `render_storage_summary` 使用。**本任务只加字典键与测试，不改任何渲染逻辑。**

## Requirements

### 1. 在 `Locale::text` 中新增以下键（中文加到中文 NVMe 块之后、英文加到英文 NVMe 块之后，保持与现有键一致的格式与缩进）

| key | 中文 (ZhCn) | 英文 (En) |
| --- | --- | --- |
| `storage_telemetry` | `存储状态` | `Storage Devices` |
| `storage_not_detected` | `未检测到 NVMe 或 MMC 存储设备` | `No NVMe or MMC storage devices detected` |
| `storage_nvme` | `NVMe` | `NVMe` |
| `storage_emmc` | `eMMC` | `eMMC` |
| `storage_sd` | `SD 卡` | `SD Card` |
| `storage_healthy` | `正常` | `Healthy` |
| `storage_warning` | `告警` | `Warning` |
| `storage_critical` | `故障` | `Critical` |
| `storage_health` | `状态` | `Health` |
| `storage_temperature` | `温度` | `Temp` |
| `storage_endurance` | `已用寿命` | `Used Endurance` |
| `storage_spare` | `可用备用` | `Available Spare` |
| `storage_io` | `累计读写` | `Data Read/Written` |
| `storage_capacity` | `格式化容量` | `Capacity` |
| `storage_model` | `设备型号` | `Model` |
| `storage_serial` | `序列号` | `Serial` |
| `storage_manufacturer` | `厂商` | `Manufacturer` |
| `storage_firmware` | `固件` | `Firmware` |
| `storage_life_a` | `SLC 寿命` | `SLC Life` |
| `storage_life_b` | `MLC 寿命` | `MLC Life` |
| `storage_pre_eol` | `预警` | `Pre-EOL` |
| `storage_eol_normal` | `正常` | `Normal` |
| `storage_eol_warning` | `预警(80%)` | `Warning(80%)` |
| `storage_eol_urgent` | `紧急` | `Urgent` |
| `storage_eol_undefined` | `未定义` | `Undefined` |
| `storage_na` | `不支持` | `N/A` |
| `storage_more_devices` | `更多设备` | `more device(s)` |
| `storage_read` | `读` | `Read` |
| `storage_written` | `写` | `Written` |

注意：
- 中文字符串末尾**不要**加句号或额外标点，与现有 NVMe 中文键风格一致（如 `nvme_healthy => "正常"`）。
- 英文 `storage_na => "N/A"`、`storage_eol_undefined => "Undefined"` 等保持大写首字母风格，与现有 `nvme_healthy => "Healthy"` 一致。
- `storage_not_detected` 的英文以句号结尾（参照 `nvme_not_detected` 英文 `"...uninitialized."`），即 `No NVMe or MMC storage devices detected.`（**带句号**）；中文不带句号。

### 2. 单元测试（`i18n.rs` 的 `#[cfg(test)]`，紧挨 `test_nvme_tui_dictionary_keys` 之后新增）

新增 `test_storage_tui_dictionary_keys`，结构与 `test_nvme_tui_dictionary_keys` 完全一致：
```rust
#[test]
fn test_storage_tui_dictionary_keys() {
    let expected = [
        ("storage_telemetry", "存储状态", "Storage Devices"),
        ("storage_not_detected", "未检测到 NVMe 或 MMC 存储设备", "No NVMe or MMC storage devices detected."),
        ("storage_nvme", "NVMe", "NVMe"),
        ("storage_emmc", "eMMC", "eMMC"),
        ("storage_sd", "SD 卡", "SD Card"),
        ("storage_healthy", "正常", "Healthy"),
        ("storage_warning", "告警", "Warning"),
        ("storage_critical", "故障", "Critical"),
        ("storage_health", "状态", "Health"),
        ("storage_temperature", "温度", "Temp"),
        ("storage_endurance", "已用寿命", "Used Endurance"),
        ("storage_spare", "可用备用", "Available Spare"),
        ("storage_io", "累计读写", "Data Read/Written"),
        ("storage_capacity", "格式化容量", "Capacity"),
        ("storage_model", "设备型号", "Model"),
        ("storage_serial", "序列号", "Serial"),
        ("storage_manufacturer", "厂商", "Manufacturer"),
        ("storage_firmware", "固件", "Firmware"),
        ("storage_life_a", "SLC 寿命", "SLC Life"),
        ("storage_life_b", "MLC 寿命", "MLC Life"),
        ("storage_pre_eol", "预警", "Pre-EOL"),
        ("storage_eol_normal", "正常", "Normal"),
        ("storage_eol_warning", "预警(80%)", "Warning(80%)"),
        ("storage_eol_urgent", "紧急", "Urgent"),
        ("storage_eol_undefined", "未定义", "Undefined"),
        ("storage_na", "不支持", "N/A"),
        ("storage_more_devices", "更多设备", "more device(s)"),
        ("storage_read", "读", "Read"),
        ("storage_written", "写", "Written"),
    ];
    for (key, zh, en) in expected {
        assert_eq!(Locale::ZhCn.text(key), zh, "ZhCn translation for {key}");
        assert_eq!(Locale::En.text(key), en, "En translation for {key}");
    }
}
```

### 3. 验证与提交
- **正确的测试包名是 `rsetup-next`**（`crates/rsetup-app/Cargo.toml` 中 `[package] name = "rsetup-next"`）。
- 运行 `PATH="/usr/bin:$PATH" cargo test -p rsetup-next i18n` 全部通过（不得回归现有 i18n 测试，包括 `test_nvme_tui_dictionary_keys`）。
- 再运行 `PATH="/usr/bin:$PATH" cargo test -p rsetup-next` 确认整个 app crate 无回归。
- 提交信息：`feat(app): add unified storage TUI dictionary keys`

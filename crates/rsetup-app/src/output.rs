use anyhow::Result;
use rsetup_core::{Controller, ExecutionPolicy, SourceApplyResult, SourcePlan, SourceStatus};

use crate::i18n::Locale;

pub(crate) fn print_source_status(status: &SourceStatus, locale: Locale, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(status)?);
        return Ok(());
    }
    println!("{} · {}", status.distribution_name, status.architecture);
    println!(
        "{}: {} · {}: {}",
        locale.text("system_sources"),
        status.current_system_provider.as_deref().unwrap_or("--"),
        locale.text("radxa_sources"),
        status.current_radxa_provider.as_deref().unwrap_or("--")
    );
    println!("{}:", locale.text("managed_source_files"));
    for file in &status.files {
        println!(
            "- {} · {} · {}",
            file.path, file.format, file.managed_entries
        );
    }
    println!("{}:", locale.text("mirror_providers"));
    for provider in &status.providers {
        println!(
            "- {:<10} {} · {}",
            provider.id, provider.name, provider.location
        );
    }
    Ok(())
}

pub(crate) fn print_source_plan(plan: &SourcePlan, locale: Locale, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
    }
    println!(
        "{}: {} ({})",
        locale.text("selected_mirror"),
        plan.provider.name,
        plan.provider.id
    );
    println!("{}: {}", locale.text("source_plan_token"), plan.plan_token);
    if plan.changes.is_empty() {
        println!("{}", locale.text("no_source_changes"));
    }
    for change in &plan.changes {
        println!(
            "\n{} · {} {}",
            change.path,
            change.replacements,
            locale.text("replacements")
        );
        for (before, after) in change.before.iter().zip(&change.after) {
            println!("- {before}\n+ {after}");
        }
    }
    for warning in &plan.warnings {
        println!("! {}", locale.source_warning(warning));
    }
    Ok(())
}

pub(crate) fn print_source_apply(
    result: &SourceApplyResult,
    locale: Locale,
    json: bool,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }
    println!(
        "{}: {}",
        locale.action_title(&result.run.action_id, &result.run.action_title),
        locale.run_summary(&result.run)
    );
    if !result.backups.is_empty() {
        println!("{}:", locale.text("backup_files"));
        for backup in &result.backups {
            println!("- {backup}");
        }
    }
    if let Some(output) = &result.run.output {
        println!("{}:\n{output}", locale.text("raw_output"));
    }
    Ok(())
}

pub(crate) fn print_status(controller: &Controller, locale: Locale, json: bool) -> Result<()> {
    let snapshot = controller.snapshot()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    let memory = snapshot
        .metrics
        .memory_percent()
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| locale.text("not_available").into());
    println!(
        "{} · {}",
        snapshot.identity.product, snapshot.identity.hostname
    );
    println!(
        "{} / {} / {}",
        snapshot.identity.operating_system,
        snapshot.identity.kernel,
        snapshot.identity.architecture
    );
    let temperature = snapshot
        .metrics
        .temperature_c
        .map(|value| format!("{value:.1}°C"))
        .unwrap_or_else(|| locale.text("not_available").into());
    let cpu = snapshot
        .metrics
        .cpu_percent
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| locale.text("not_available").into());
    if locale.is_zh() {
        println!("处理器 {cpu:>6}   内存 {memory:>6}   温度 {temperature}");
    } else {
        println!("CPU {cpu:>6}   MEM {memory:>6}   TEMP {temperature}");
    }
    println!(
        "{} {} · {} {} · {} {}{}",
        snapshot.interfaces.len(),
        locale.text("network_interfaces"),
        snapshot
            .capabilities
            .iter()
            .filter(|cap| cap.available)
            .count(),
        locale.text("capability_signals"),
        snapshot.alerts.len(),
        locale.text("alerts"),
        if snapshot.synthetic {
            format!(" · {}", locale.text("synthetic_data"))
        } else {
            String::new()
        }
    );
    Ok(())
}

pub(crate) fn print_actions(controller: &Controller, locale: Locale, json: bool) -> Result<()> {
    let actions = controller.actions();
    if json {
        println!("{}", serde_json::to_string_pretty(&actions)?);
        return Ok(());
    }
    for action in actions {
        let unavailable = if action.available {
            String::new()
        } else {
            format!(
                " [{}: {}]",
                locale.text("unavailable"),
                locale.action_unavailable_reason(
                    action.unavailable_reason.as_deref().unwrap_or("--")
                )
            )
        };
        println!(
            "{:<36} {:<9} {}{}{}",
            action.id,
            locale.risk(action.risk),
            locale.action_title(&action.id, &action.title),
            if action.requires_root {
                format!(" [{}]", locale.text("root"))
            } else {
                String::new()
            },
            unavailable,
        );
    }
    Ok(())
}

pub(crate) fn print_doctor(controller: &Controller, locale: Locale, json: bool) -> Result<()> {
    let snapshot = controller.snapshot()?;
    let checks: Vec<(&str, bool, &str)> = vec![
        (
            "probe",
            true,
            if snapshot.synthetic {
                "synthetic demo provider"
            } else {
                "live Linux provider"
            },
        ),
        (
            "execution",
            true,
            match controller.policy() {
                ExecutionPolicy::DryRun => "dry-run guard enabled",
                ExecutionPolicy::Live => "LIVE changes enabled",
            },
        ),
        (
            "native-actions",
            true,
            "built into the rsetup-next control plane",
        ),
        (
            "device-tree",
            std::path::Path::new("/proc/device-tree/model").exists(),
            "SBC model probe",
        ),
    ];
    if json {
        let value: Vec<_> = checks
            .iter()
            .map(|(id, ready, detail)| {
                serde_json::json!({"id": id, "ready": ready, "detail": detail})
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        for (id, ready, detail) in checks {
            let label = match id {
                "probe" => locale.text("probe"),
                "execution" => locale.text("execution"),
                "native-actions" => locale.text("native_actions"),
                "device-tree" => locale.text("device_tree"),
                _ => id,
            };
            let detail = match detail {
                "synthetic demo provider" => locale.text("synthetic_provider"),
                "live Linux provider" => locale.text("live_provider"),
                "dry-run guard enabled" => locale.text("dry_run_enabled"),
                "LIVE changes enabled" => locale.text("live_changes_enabled"),
                "built into the rsetup-next control plane" => locale.text("native_actions_ready"),
                "SBC model probe" => locale.text("board_model_probe"),
                _ => detail,
            };
            println!(
                "{} {:<14} {}",
                if ready {
                    locale.text("ready")
                } else {
                    locale.text("unavailable")
                },
                label,
                detail
            );
        }
    }
    Ok(())
}

pub(crate) fn print_json_or_debug<T>(value: &T, json: bool) -> Result<()>
where
    T: serde::Serialize + std::fmt::Debug,
{
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{value:#?}");
    }
    Ok(())
}

pub(crate) fn decode_base64(value: &str) -> Option<Vec<u8>> {
    fn decode(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    if !value.len().is_multiple_of(4) {
        return None;
    }
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    let chunk_count = value.len() / 4;
    for (index, chunk) in value.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        if (chunk.contains(&b'=') && index + 1 != chunk_count)
            || (chunk[2] == b'=' && chunk[3] != b'=')
        {
            return None;
        }
        let a = u32::from(decode(chunk[0])?);
        let b = u32::from(decode(chunk[1])?);
        let c = if chunk[2] == b'=' {
            0
        } else {
            u32::from(decode(chunk[2])?)
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            u32::from(decode(chunk[3])?)
        };
        if (chunk[2] == b'=' && b & 15 != 0) || (chunk[3] == b'=' && chunk[2] != b'=' && c & 3 != 0)
        {
            return None;
        }
        let bits = (a << 18) | (b << 12) | (c << 6) | d;
        output.push((bits >> 16) as u8);
        if chunk[2] != b'=' {
            output.push((bits >> 8) as u8);
        }
        if chunk[3] != b'=' {
            output.push(bits as u8);
        }
    }
    Some(output)
}

#[cfg(test)]
mod decoding_tests {
    use super::decode_base64;

    #[test]
    fn base64_requires_canonical_padding() {
        for invalid in ["AB=C", "Zg==AAAA", "Zh==", "Zm9=", "====", "A===", "abc"] {
            assert!(decode_base64(invalid).is_none(), "{invalid}");
        }
        assert_eq!(decode_base64("Zg==").unwrap(), b"f");
        assert_eq!(decode_base64("Zm8=").unwrap(), b"fo");
        assert_eq!(decode_base64("Zm9v").unwrap(), b"foo");
    }
}
